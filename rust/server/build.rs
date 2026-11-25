// The web client (../web/dist, built by trunk) is embedded in the server. Without it the
// server still builds, with a page that says how to build the client.
fn main() {
    let dist = std::path::Path::new("../web/dist");
    println!("cargo:rerun-if-changed=../web/dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("creating ../web/dist");
        std::fs::write(dist.join("index.html"), "<!doctype html><meta charset=utf-8><title>cwbcode</title><body style=\"font-family:system-ui;padding:3rem\"><h1>cwbcode</h1><p>The web client has not been built. Run <code>make build</code>.</p>").expect("writing the placeholder page");
    }
}
