//! The local knowledge base: saving text, files and images, and hybrid search over them.
//!
//! Search runs two rankings and merges them by reciprocal rank fusion:
//!   - BM25 over an FTS5 index of the chunks (exact words; prefixes, so Korean particles and
//!     English endings do not hide a match), and
//!   - semantic: cosine similarity between embeddings. An embedding is the model's own output
//!     state after it has read the text and is about to say what the text is about, so no
//!     second model is needed and a Korean question finds an English note.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::llm::Engine;
use crate::prompt::Piece;
use crate::store::{Chunk, Store};

/// Names the embedding recipe; stored with every vector, so a changed recipe re-embeds.
pub const EMBEDDER: &str = "inkling2-last-topic-v1";
const EMBED_INSTRUCTION: &str = "\n\nWhat is the text above about? Answer with a few keywords in English.";
/// Characters of a text that an embedding reads.
const EMBED_CHARS: usize = 6000;
const CHUNK_CHARS: usize = 1400;
const CHUNK_OVERLAP: usize = 200;
/// Characters of a text that are indexed (the whole text is stored): each chunk costs a pass
/// of the model, so a text of this size takes a few minutes.
pub const MAX_INDEX_CHARS: usize = 1_500_000;
/// Largest file kept in the store.
pub const MAX_FILE_BYTES: u64 = 64 << 20;
/// Reciprocal rank fusion constant.
const RRF_K: f64 = 60.0;
/// A semantic match counts when its similarity is at least this share of the best one's.
const SEMANTIC_FLOOR: f32 = 0.35;
/// Words too common to search for by keyword.
const STOPWORDS: [&str; 76] = [
    "a", "an", "the", "and", "or", "but", "if", "of", "to", "in", "on", "at", "by", "for", "with", "from", "as", "is", "are", "was", "were", "be", "been", "it", "its", "this", "that",
    "these", "those", "i", "me", "my", "we", "our", "us", "you", "your", "he", "she", "they", "them", "their", "do", "does", "did", "have", "has", "had", "can", "could", "will", "would",
    "should", "what", "when", "where", "which", "who", "whom", "why", "how", "not", "no", "so", "than", "then", "there", "here", "about", "into", "s", "t", "am", "also", "any", "all",
];
/// Texts of many kinds, embedded once: what their embeddings have in common is what every
/// embedding has in common and says nothing, so it is subtracted before comparing.
const BACKGROUND: [&str; 16] = [
    "The quarterly report shows revenue grew eight percent while operating costs stayed flat.",
    "fn main() { let args: Vec<String> = std::env::args().collect(); println!(\"{}\", args.len()); }",
    "Preheat the oven to 200 degrees, slice the potatoes thinly and bake them for forty minutes.",
    "내일 오전 회의는 10시에 3층 회의실에서 열립니다. 자료는 미리 공유해 주세요.",
    "The patient reported mild headaches and was advised to rest and drink more water.",
    "SELECT name, count(*) FROM orders WHERE created > '2024-01-01' GROUP BY name ORDER BY 2 DESC;",
    "A photo of a mountain lake at sunrise, with pine trees reflected in the still water.",
    "The treaty was signed in 1648 and ended decades of war across central Europe.",
    "Remember to renew the car insurance before the end of the month.",
    "Electrons occupy discrete energy levels, and light is emitted when they fall to a lower one.",
    "def mean(xs): return sum(xs) / len(xs) if xs else 0.0",
    "오늘 저녁에는 비가 올 예정이니 우산을 챙기세요.",
    "The striker scored twice in the second half and the home team won the cup final.",
    "To install the package, run the setup script and add the bin folder to your PATH.",
    "She played the cello in a small orchestra that toured the coastal towns every summer.",
    "A screenshot of a settings window with a dark theme and a list of toggle switches.",
];

pub fn embed(engine: &Engine, text: &str) -> Result<Vec<f32>> {
    let text: String = text.chars().take(EMBED_CHARS).collect();
    let c = |s: &str| Piece::Control(s.to_string());
    let ids = engine.encode(&[
        c("<|message_system|><|content_text|>"),
        Piece::Text("Thinking effort level: 0".into()),
        c("<|end_message|><|message_user|><|content_text|>"),
        Piece::Text(text),
        Piece::Text(EMBED_INSTRUCTION.into()),
        c("<|end_message|><|message_model|><|content_text|>"),
    ])?;
    let (rows, width) = engine.hidden_rows(&ids)?;
    Ok(rows[rows.len() - width..].to_vec())
}

/// Split a text into overlapping chunks at paragraph or line ends.
pub fn chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + CHUNK_CHARS).min(chars.len());
        if end < chars.len() {
            // Prefer to end at a blank line, then a line end, then a space, in the last third.
            let floor = start + CHUNK_CHARS * 2 / 3;
            let find = |pred: &dyn Fn(usize) -> bool| (floor..end).rev().find(|&i| pred(i));
            if let Some(i) = find(&|i| chars[i] == '\n' && chars[i - 1] == '\n').or_else(|| find(&|i| chars[i] == '\n')).or_else(|| find(&|i| chars[i] == ' ')) {
                end = i + 1;
            }
        }
        let piece: String = chars[start..end].iter().collect();
        if !piece.trim().is_empty() {
            out.push(piece.trim().to_string());
        }
        if end == chars.len() {
            break;
        }
        // The next chunk repeats the end of this one, from the start of a line if there is one.
        let back = end.saturating_sub(CHUNK_OVERLAP).max(start + 1);
        start = (back..end).find(|&i| chars[i - 1] == '\n').unwrap_or(back);
    }
    out
}

/// The words of a query worth searching for: lowercased, without the commonest ones.
pub fn terms(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()) && seen.insert(w.clone()))
        .take(48)
        .collect()
}

/// An FTS5 query for free text: every word quoted (so no character is query syntax), as a
/// prefix, any of them matching.
pub fn fts_query(text: &str) -> String {
    terms(text).iter().map(|w| format!("\"{w}\"*")).collect::<Vec<_>>().join(" OR ")
}

/// The share of the query's words that a text contains (as word prefixes, as FTS5 matches).
pub fn coverage(query_terms: &[String], text: &str) -> f64 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    query_terms.iter().filter(|t| words.iter().any(|w| w.starts_with(t.as_str()))).count() as f64 / query_terms.len() as f64
}

fn mime_of(path: &Path, data: &[u8]) -> &'static str {
    match image::guess_format(data) {
        Ok(image::ImageFormat::Png) => return "image/png",
        Ok(image::ImageFormat::Jpeg) => return "image/jpeg",
        Ok(image::ImageFormat::WebP) => return "image/webp",
        Ok(image::ImageFormat::Gif) => return "image/gif",
        Ok(image::ImageFormat::Bmp) => return "image/bmp",
        _ => {}
    }
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "pdf" => "application/pdf",
        "json" => "application/json",
        _ if std::str::from_utf8(data).is_ok() => "text/plain",
        _ => "application/octet-stream",
    }
}

pub fn is_image(mime: &str) -> bool {
    mime.starts_with("image/")
}

/// Store a file's bytes. Returns (blob id, mime).
pub fn store_file(store: &Store, path: &Path) -> Result<(i64, &'static str, Vec<u8>)> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a file", path.display());
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!("{} is {} MB; the store takes files up to {} MB", path.display(), meta.len() >> 20, MAX_FILE_BYTES >> 20);
    }
    let data = std::fs::read(path)?;
    let mime = mime_of(path, &data);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    Ok((store.put_blob(&name, mime, &data)?, mime, data))
}

/// Embed chunks. An interrupt stops it: what is left is embedded by the next search (until
/// then those chunks are found by BM25).
fn embed_chunks(store: &Store, engine: &Engine, chunks: &[(i64, String)]) -> Result<usize> {
    let mut done = 0;
    for (id, text) in chunks {
        if crate::agent::interrupted() {
            break;
        }
        store.set_vector(*id, EMBEDDER, &embed(engine, text)?)?;
        done += 1;
    }
    Ok(done)
}

/// Save an entry and index it. `text` is what search reads: a note, a text file's contents,
/// or an image's description. With an engine the chunks are embedded now; without one they
/// are found by BM25 until the next search with the model embeds them.
pub fn save(store: &Store, engine: Option<&Engine>, kind: &str, title: &str, source: &str, text: &str, blob: Option<i64>) -> Result<i64> {
    let doc = store.add_doc(kind, title, source, text, blob)?;
    let indexed: String = text.chars().take(MAX_INDEX_CHARS).collect();
    let mut parts = chunks(&indexed);
    if parts.is_empty() {
        parts.push(title.to_string());
    }
    let mut added = Vec::new();
    for (seq, part) in parts.iter().enumerate() {
        added.push((store.add_chunk(doc, seq as i64, part, title)?, embed_input(title, part)));
    }
    if let Some(engine) = engine {
        embed_chunks(store, engine, &added)?;
    }
    Ok(doc)
}

fn embed_input(title: &str, text: &str) -> String {
    if title.is_empty() || text.starts_with(title) { text.to_string() } else { format!("{title}\n\n{text}") }
}

/// Embed the chunks that have no embedding yet. Returns how many were embedded.
pub fn embed_missing(store: &Store, engine: &Engine) -> Result<usize> {
    let missing: Vec<(i64, String)> = store.chunks_without_vector(EMBEDDER)?.into_iter().map(|c| (c.id, embed_input(&c.title, &c.text))).collect();
    embed_chunks(store, engine, &missing)
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk: Chunk,
    pub score: f64,
    /// Rank in the BM25 list and in the semantic list (from 1), where it appears.
    pub bm25_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
    pub cosine: Option<f32>,
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
}

/// The mean of the background texts' embeddings (made once per store and recipe).
pub fn background(store: &Store, engine: &Engine) -> Result<Vec<f32>> {
    let key = format!("background:{EMBEDDER}");
    if let Some(bytes) = store.meta(&key)? {
        return Ok(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect());
    }
    let mut mean: Vec<f32> = Vec::new();
    for text in BACKGROUND {
        let v = embed(engine, text)?;
        mean.resize(v.len(), 0.0);
        mean.iter_mut().zip(&v).for_each(|(m, x)| *m += x / BACKGROUND.len() as f32);
    }
    store.set_meta(&key, &mean.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>())?;
    Ok(mean)
}

/// Chunks ranked by cosine similarity to `query`, best first, after subtracting `common`
/// (the part all embeddings share). Only matches near the best one are kept, so that a
/// small knowledge base does not rank everything as a match.
pub fn semantic_rank(mut vectors: Vec<(i64, Vec<f32>)>, mut query: Vec<f32>, common: &[f32], limit: usize) -> Vec<(i64, f32)> {
    vectors.retain(|(_, v)| v.len() == query.len());
    if common.len() == query.len() {
        for v in vectors.iter_mut().map(|(_, v)| v).chain(std::iter::once(&mut query)) {
            v.iter_mut().zip(common).for_each(|(x, m)| *x -= m);
        }
    }
    normalize(&mut query);
    let mut scored: Vec<(i64, f32)> = vectors
        .into_iter()
        .map(|(id, mut v)| {
            normalize(&mut v);
            (id, v.iter().zip(&query).map(|(a, b)| a * b).sum())
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let floor = scored.first().map_or(0.0, |best| (best.1 * SEMANTIC_FLOOR).max(0.0));
    scored.retain(|s| s.1 > 0.0 && s.1 >= floor);
    scored.truncate(limit);
    scored
}

/// Merge ranked lists of (chunk id, weight) by reciprocal rank fusion: a chunk scores
/// weight / (60 + rank) in each list it is in.
pub fn fuse(lists: &[&[(i64, f64)]]) -> Vec<(i64, f64)> {
    let mut score: HashMap<i64, f64> = HashMap::new();
    for list in lists {
        for (rank, (id, weight)) in list.iter().enumerate() {
            *score.entry(*id).or_default() += weight / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(i64, f64)> = score.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Hybrid search: BM25 and (with an engine) semantic, fused. One hit per entry: its best chunk.
/// A keyword match weighs as much as the share of the query's words it holds, so a chunk that
/// shares one incidental word with a long question does not outrank what the question means.
pub fn search(store: &Store, engine: Option<&Engine>, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let pool = (limit * 5).max(30);
    let words = terms(query);
    let mut lexical: Vec<(i64, f64)> = Vec::new();
    for (id, _) in store.bm25(&fts_query(query), pool)? {
        if let Some(chunk) = store.chunk(id)? {
            lexical.push((id, coverage(&words, &format!("{} {}", chunk.title, chunk.text))));
        }
    }
    let semantic: Vec<(i64, f32)> = match engine {
        Some(engine) => {
            embed_missing(store, engine)?;
            semantic_rank(store.vectors(EMBEDDER)?, embed(engine, query)?, &background(store, engine)?, pool)
        }
        None => vec![],
    };
    let semantic_weighted: Vec<(i64, f64)> = semantic.iter().map(|(id, _)| (*id, 1.0)).collect();
    let mut hits = Vec::new();
    let mut seen_docs = std::collections::HashSet::new();
    for (id, score) in fuse(&[&lexical, &semantic_weighted]) {
        let Some(chunk) = store.chunk(id)? else { continue };
        if !seen_docs.insert(chunk.doc) {
            continue;
        }
        let semantic_rank = semantic.iter().position(|(c, _)| *c == id);
        hits.push(Hit {
            chunk,
            score,
            bm25_rank: lexical.iter().position(|(c, _)| *c == id).map(|r| r + 1),
            semantic_rank: semantic_rank.map(|r| r + 1),
            cosine: semantic_rank.map(|r| semantic[r].1),
        });
        if hits.len() == limit {
            break;
        }
    }
    Ok(hits)
}

/// Hits as text, for the model and for the terminal.
pub fn format_hits(hits: &[Hit], snippet_chars: usize) -> String {
    if hits.is_empty() {
        return "No matches in the knowledge base.".to_string();
    }
    let mut out = String::new();
    for hit in hits {
        let how = match (hit.bm25_rank, hit.semantic_rank) {
            (Some(b), Some(s)) => format!("bm25 #{b}, semantic #{s}"),
            (Some(b), None) => format!("bm25 #{b}"),
            (None, Some(s)) => format!("semantic #{s}"),
            (None, None) => String::new(),
        };
        let snippet: String = hit.chunk.text.chars().take(snippet_chars).collect();
        let more = if hit.chunk.text.chars().count() > snippet_chars { " ..." } else { "" };
        out.push_str(&format!("[#{} {} \"{}\" · {how}]\n{snippet}{more}\n\n", hit.chunk.doc, hit.chunk.kind, hit.chunk.title));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_chunked_with_overlap_at_line_ends() {
        assert_eq!(chunks("short note"), vec!["short note"]);
        assert!(chunks("  \n ").is_empty());
        let text: String = (0..200).map(|i| format!("line number {i} of the document\n")).collect();
        let parts = chunks(&text);
        assert!(parts.len() > 3);
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK_CHARS && p.starts_with("line number") && p.ends_with("document")));
        // Every line is in some chunk, and neighbours share lines.
        assert!((0..200).all(|i| parts.iter().any(|p| p.contains(&format!("line number {i} of")))));
        let last_of_first = parts[0].lines().last().unwrap();
        assert!(parts[1].contains(last_of_first));
        // A text without any break is still split.
        assert!(chunks(&"x".repeat(5000)).len() >= 4);
    }

    #[test]
    fn a_query_becomes_quoted_prefix_terms() {
        assert_eq!(fts_query("What's the Docker-image tag?"), "\"docker\"* OR \"image\"* OR \"tag\"*");
        assert_eq!(fts_query("비밀번호 재설정 \"AND\" (x:y)"), "\"비밀번호\"* OR \"재설정\"* OR \"x\"* OR \"y\"*");
        assert_eq!(fts_query(" -- "), "");
        // Whatever is typed is a valid query.
        let s = Store::in_memory().unwrap();
        for q in ["a AND", "\"", "NEAR(", "x*y^2", "col:val -not", "'; DROP TABLE docs;--"] {
            s.bm25(&fts_query(q), 5).unwrap();
        }
    }

    #[test]
    fn fusion_prefers_what_both_rankings_like_and_weighs_keyword_matches() {
        let full = |ids: &[i64]| ids.iter().map(|&i| (i, 1.0)).collect::<Vec<_>>();
        let fused = fuse(&[&full(&[1, 2, 3]), &full(&[3, 2, 9])]);
        // 3 is third and first, 2 is second in both: 1/63 + 1/61 beats 2/62.
        assert_eq!(fused.iter().map(|f| f.0).collect::<Vec<_>>(), vec![3, 2, 1, 9]);
        // A keyword hit on one word in seven loses to the best semantic match.
        let fused = fuse(&[&[(5, 1.0 / 7.0)], &full(&[8])]);
        assert_eq!(fused[0].0, 8);
        let words = terms("code to look up an element in a sorted array");
        assert_eq!(words, ["code", "look", "up", "element", "sorted", "array"]);
        assert_eq!(coverage(&words, "Staff may carry over up to five days"), 1.0 / 6.0);
        assert_eq!(coverage(&terms("배포 담당자"), "우리 팀 배포는 목요일, 담당자는 민수"), 1.0);
        assert_eq!(coverage(&[], "anything"), 0.0);
    }

    #[test]
    fn semantic_ranking_is_by_cosine_after_removing_the_common_part() {
        // Nothing in common: plain cosine; a vector of another width is left out, and so is
        // what points away from the query.
        let ranked = semantic_rank(vec![(1, vec![1.0, 0.0]), (2, vec![0.0, 1.0]), (3, vec![1.0, 1.0, 1.0]), (4, vec![-1.0, 0.0])], vec![0.9, 0.5], &[], 5);
        assert_eq!(ranked.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 2]);
        // A large common part: only the small differences decide, and weak matches are dropped.
        let common = [100.0f32, 100.0, 100.0];
        let vectors = vec![(1, vec![101.0, 100.0, 100.0]), (2, vec![100.0, 101.0, 100.0]), (3, vec![100.9, 100.3, 100.0]), (4, vec![100.1, 100.0, 101.0])];
        let ranked = semantic_rank(vectors.clone(), vec![101.0, 100.1, 100.0], &common, 5);
        assert_eq!(ranked.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 3]);
        // Without the subtraction everything looks alike and nothing is told apart.
        let blurred = semantic_rank(vectors, vec![101.0, 100.1, 100.0], &[], 5);
        assert_eq!(blurred.len(), 4);
        assert!(blurred[0].1 - blurred[3].1 < 0.001);
    }

    /// Needs the model (20 GB): `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn with_the_model_meaning_is_found_where_no_word_matches() {
        let engine = Engine::load(crate::llm::DEFAULT_MODEL, crate::llm::Sampler { temp: 0.0, top_k: 0 }).unwrap();
        let s = Store::in_memory().unwrap();
        let notes = [
            ("Password reset", "To reset your password, open Settings, choose Account, and click 'Forgot password'. A reset link is emailed to you."),
            ("배포 일정", "우리 팀 배포는 매주 목요일 오후 3시에 하고, 담당자는 민수입니다."),
            ("Vacation policy", "Our office is closed on public holidays. Staff may carry over up to five unused vacation days into the next year."),
            ("Everest", "Mount Everest, at 8,849 metres, is the highest mountain above sea level, on the border of Nepal and China."),
            ("Invoice", "Invoice 2291 from Acme Corp for 4,200 dollars is due on March 3rd; payment by bank transfer only."),
            ("Binary search", "fn find(v: &[i32], x: i32) -> Option<usize> { let (mut lo, mut hi) = (0, v.len()); while lo < hi { let mid = (lo + hi) / 2; if v[mid] < x { lo = mid + 1 } else { hi = mid } } (lo < v.len() && v[lo] == x).then_some(lo) }"),
        ];
        let ids: Vec<i64> = notes.iter().map(|(title, text)| save(&s, Some(&engine), "text", title, "", text, None).unwrap()).collect();
        for (query, want) in [
            ("I can't log in because I lost my credentials", 0),
            ("when do we ship to production and who owns it", 1),
            ("how many leave days roll over", 2),
            ("세계에서 가장 높은 산은?", 3),
            ("how much do we owe the supplier", 4),
            ("code to look up an element in a sorted array quickly", 5),
            ("비밀번호를 잊어버렸어요", 0),
        ] {
            let hits = search(&s, Some(&engine), query, 3).unwrap();
            assert_eq!(hits.first().map(|h| h.chunk.doc), Some(ids[want]), "{query}: {}", format_hits(&hits, 60));
        }
        // A keyword still finds its note, and both rankings agree on it.
        let hits = search(&s, Some(&engine), "invoice 2291", 3).unwrap();
        assert_eq!((hits[0].chunk.doc, hits[0].bm25_rank, hits[0].semantic_rank), (ids[4], Some(1), Some(1)));
    }

    #[test]
    fn hybrid_search_without_a_model_is_bm25_and_groups_by_entry() {
        let s = Store::in_memory().unwrap();
        let a = save(&s, None, "text", "Deploy notes", "", "The pipeline pushes a Docker image to the registry.", None).unwrap();
        let long: String = (0..120).map(|i| format!("Paragraph {i} mentions the registry mirror.\n")).collect();
        let b = save(&s, None, "file", "mirror.md", "/tmp/mirror.md", &long, None).unwrap();
        save(&s, None, "text", "Recipe", "", "Kimchi stew with tofu.", None).unwrap();
        let hits = search(&s, None, "registry", 5).unwrap();
        let mut docs: Vec<i64> = hits.iter().map(|h| h.chunk.doc).collect();
        docs.sort();
        assert_eq!(docs, vec![a, b]);
        assert!(hits.iter().all(|h| h.bm25_rank.is_some() && h.semantic_rank.is_none()));
        assert!(format_hits(&hits, 80).contains("\"Deploy notes\" · bm25 #"));
        assert_eq!(format_hits(&search(&s, None, "volcano", 5).unwrap(), 80), "No matches in the knowledge base.");
        assert_eq!(s.chunks_without_vector(EMBEDDER).unwrap().len() as i64, s.counts().unwrap().1);
    }
}
