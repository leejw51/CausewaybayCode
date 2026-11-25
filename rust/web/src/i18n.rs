//! The web client's words in eight languages. What the backend and the model say (errors,
//! approvals, tool output, answers) arrives as it is; this is the interface around it.

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    En,
    Ko,
    Cs,
    Ja,
    Yue,
    Zh,
    Es,
    Tr,
}

impl Lang {
    pub const ALL: [Lang; 8] = [Lang::En, Lang::Ko, Lang::Cs, Lang::Ja, Lang::Yue, Lang::Zh, Lang::Es, Lang::Tr];

    /// The code kept in preferences and put on the page (`<html lang>`).
    pub fn code(self) -> &'static str {
        ["en", "ko", "cs", "ja", "yue", "zh", "es", "tr"][self as usize]
    }

    /// The language's own name.
    pub fn name(self) -> &'static str {
        ["English", "한국어", "Čeština", "日本語", "廣東話", "中文", "Español", "Türkçe"][self as usize]
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code() == code)
    }

    /// The browser's language, if it is one of ours.
    pub fn from_browser(tag: &str) -> Lang {
        let tag = tag.to_lowercase();
        if tag.starts_with("zh-hk") || tag.starts_with("zh-mo") || tag.starts_with("yue") {
            return Lang::Yue;
        }
        Lang::ALL.into_iter().find(|l| tag.starts_with(l.code())).unwrap_or(Lang::En)
    }
}

/// The text for `key` in `lang` (English if the key has no entry, the key itself if unknown).
pub fn tr(lang: Lang, key: &str) -> &'static str {
    match TEXTS.iter().find(|(k, _)| *k == key) {
        Some((_, texts)) => texts[lang as usize],
        None => "?",
    }
}

/// `tr`, with `{}` replaced by `value`.
pub fn tr_with(lang: Lang, key: &str, value: &str) -> String {
    tr(lang, key).replacen("{}", value, 1)
}

// Order: English, Korean, Czech, Japanese, Cantonese, Chinese, Spanish, Turkish.
const TEXTS: &[(&str, [&str; 8])] = &[
    ("brand_sub", ["local AI coding agent", "로컬 AI 코딩 에이전트", "místní AI agent pro programování", "ローカルAIコーディングエージェント", "本機 AI 編程助手", "本地 AI 编程助手", "agente de programación con IA local", "yerel yapay zekâ kodlama ajanı"]),
    ("nav_dashboard", ["Dashboard", "대시보드", "Přehled", "ダッシュボード", "儀表板", "仪表盘", "Panel", "Pano"]),
    ("nav_chat", ["Chat", "채팅", "Chat", "チャット", "對話", "对话", "Chat", "Sohbet"]),
    ("nav_knowledge", ["Knowledge", "지식", "Znalosti", "ナレッジ", "知識庫", "知识库", "Conocimiento", "Bilgi"]),
    ("nav_tools", ["Tools", "도구", "Nástroje", "ツール", "工具", "工具", "Herramientas", "Araçlar"]),
    ("projects", ["Projects", "프로젝트", "Projekty", "プロジェクト", "專案", "项目", "Proyectos", "Projeler"]),
    ("conversations", ["Conversations", "대화", "Konverzace", "会話", "對話記錄", "会话", "Conversaciones", "Konuşmalar"]),
    ("new", ["+ new", "+ 새로", "+ nová", "+ 新規", "+ 新", "+ 新建", "+ nueva", "+ yeni"]),
    ("open", ["Open", "열기", "Otevřít", "開く", "開啟", "打开", "Abrir", "Aç"]),
    ("open_ph", ["folder to open or create", "열거나 만들 폴더", "složka k otevření nebo vytvoření", "開く・作成するフォルダ", "要開或者新建嘅資料夾", "要打开或新建的文件夹", "carpeta para abrir o crear", "açılacak veya oluşturulacak klasör"]),
    ("created", ["Created {}", "{} 폴더를 만들었습니다", "Vytvořeno: {}", "{} を作成しました", "已經建立 {}", "已创建 {}", "Se creó {}", "{} oluşturuldu"]),
    ("offline", ["backend not reachable", "백엔드에 연결할 수 없음", "backend není dostupný", "バックエンドに接続できません", "連唔到後端", "无法连接后端", "no se alcanza el backend", "arka uca ulaşılamıyor"]),
    ("working", ["working", "작업 중", "pracuje", "作業中", "做緊嘢", "工作中", "trabajando", "çalışıyor"]),
    ("local_short", ["local mode · no login", "로컬 모드 · 로그인 없음", "místní režim · bez přihlášení", "ローカルモード · ログイン不要", "本機模式 · 唔使登入", "本地模式 · 无需登录", "modo local · sin inicio de sesión", "yerel mod · giriş yok"]),
    ("sign_out", ["Sign out", "로그아웃", "Odhlásit se", "サインアウト", "登出", "退出登录", "Cerrar sesión", "Çıkış yap"]),
    ("connecting", ["Connecting to the backend ...", "백엔드에 연결하는 중 ...", "Připojování k backendu ...", "バックエンドに接続中 ...", "連緊去後端 ...", "正在连接后端 ...", "Conectando con el backend ...", "Arka uca bağlanılıyor ..."]),
    ("login_belongs", ["This backend belongs to wallet", "이 백엔드의 소유 지갑", "Tento backend patří peněžence", "このバックエンドの所有ウォレット", "呢個後端屬於錢包", "此后端属于钱包", "Este backend pertenece a la cartera", "Bu arka uç şu cüzdana ait"]),
    ("login_explain", ["Sign in with that wallet's recovery phrase. The key is derived in this browser; only a signature is sent.", "해당 지갑의 복구 문구로 로그인하세요. 키는 이 브라우저에서 만들어지고, 서명만 전송됩니다.", "Přihlaste se obnovovací frází této peněženky. Klíč se odvodí v tomto prohlížeči; odesílá se jen podpis.", "そのウォレットのリカバリーフレーズでサインインします。鍵はこのブラウザ内で導出され、送信されるのは署名だけです。", "用呢個錢包嘅助記詞登入。密鑰喺呢個瀏覽器度計出嚟，淨係會送出簽名。", "请使用该钱包的助记词登录。密钥在此浏览器中派生，只会发送签名。", "Inicia sesión con la frase de recuperación de esa cartera. La clave se deriva en este navegador; solo se envía una firma.", "O cüzdanın kurtarma ifadesiyle giriş yapın. Anahtar bu tarayıcıda türetilir; yalnızca imza gönderilir."]),
    ("phrase_ph", ["twelve or twenty-four words", "12개 또는 24개 단어", "dvanáct nebo dvacet čtyři slov", "12語または24語", "十二或者廿四個字", "12 或 24 个单词", "doce o veinticuatro palabras", "on iki veya yirmi dört kelime"]),
    ("show", ["show", "보기", "zobrazit", "表示", "顯示", "显示", "mostrar", "göster"]),
    ("account", ["account", "계정", "účet", "アカウント", "帳戶", "账户", "cuenta", "hesap"]),
    ("remember", ["remember on this device", "이 기기에 저장", "zapamatovat v tomto zařízení", "この端末に保存", "喺呢部機記住", "在此设备上记住", "recordar en este dispositivo", "bu cihazda hatırla"]),
    ("sign_in", ["Sign in", "로그인", "Přihlásit se", "サインイン", "登入", "登录", "Iniciar sesión", "Giriş yap"]),
    ("signing_in", ["Signing in ...", "로그인 중 ...", "Přihlašování ...", "サインイン中 ...", "登入緊 ...", "正在登录 ...", "Iniciando sesión ...", "Giriş yapılıyor ..."]),
    ("cert_note", ["Server certificate (compare with `cwbcode url`):", "서버 인증서 (`cwbcode url` 과 비교):", "Certifikát serveru (porovnejte s `cwbcode url`):", "サーバー証明書(`cwbcode url` と照合):", "伺服器證書(同 `cwbcode url` 對一對):", "服务器证书(与 `cwbcode url` 比对):", "Certificado del servidor (compáralo con `cwbcode url`):", "Sunucu sertifikası (`cwbcode url` ile karşılaştırın):"]),
    ("token_explain", ["Enter the backend's token. On the machine it runs on: cwbcode url", "백엔드 토큰을 입력하세요. 실행 중인 컴퓨터에서: cwbcode url", "Zadejte token backendu. Na počítači, kde běží: cwbcode url", "バックエンドのトークンを入力してください。実行中のマシンで: cwbcode url", "輸入後端嘅 token。喺行緊嗰部機度: cwbcode url", "请输入后端令牌。在运行它的机器上: cwbcode url", "Introduce el token del backend. En la máquina donde se ejecuta: cwbcode url", "Arka ucun belirtecini girin. Çalıştığı makinede: cwbcode url"]),
    ("connect", ["Connect", "연결", "Připojit", "接続", "連線", "连接", "Conectar", "Bağlan"]),
    ("d_model", ["Model", "모델", "Model", "モデル", "模型", "模型", "Modelo", "Model"]),
    ("d_memory", ["Memory", "메모리", "Paměť", "メモリ", "記憶體", "内存", "Memoria", "Bellek"]),
    ("d_context", ["Context", "컨텍스트", "Kontext", "コンテキスト", "上下文", "上下文", "Contexto", "Bağlam"]),
    ("d_transport", ["Transport", "전송", "Přenos", "トランスポート", "傳輸", "传输", "Transporte", "Aktarım"]),
    ("d_safety", ["Safety", "안전", "Bezpečnost", "安全性", "安全", "安全", "Seguridad", "Güvenlik"]),
    ("d_uptime", ["Uptime", "가동 시간", "Doba běhu", "稼働時間", "運行時間", "运行时间", "Tiempo activo", "Çalışma süresi"]),
    ("peak", ["peak {} GB", "최대 {} GB", "maximum {} GB", "ピーク {} GB", "最高 {} GB", "峰值 {} GB", "máximo {} GB", "en yüksek {} GB"]),
    ("ctx_sub", ["of {} tokens before summarizing", "요약 전 한도 {} 토큰", "z {} tokenů před shrnutím", "要約まで {} トークン", "總結之前上限 {} 個 token", "总结前上限 {} 个 token", "de {} tokens antes de resumir", "özetlemeden önce {} belirteç"]),
    ("d_offline", ["offline", "오프라인", "offline", "オフライン", "離線", "离线", "sin conexión", "çevrimdışı"]),
    ("transport_both", ["Cap'n Proto · server offers HTTP/3 and HTTP/2", "Cap'n Proto · 서버가 HTTP/3 과 HTTP/2 제공", "Cap'n Proto · server nabízí HTTP/3 a HTTP/2", "Cap'n Proto · サーバーは HTTP/3 と HTTP/2 に対応", "Cap'n Proto · 伺服器支援 HTTP/3 同 HTTP/2", "Cap'n Proto · 服务器支持 HTTP/3 和 HTTP/2", "Cap'n Proto · el servidor ofrece HTTP/3 y HTTP/2", "Cap'n Proto · sunucu HTTP/3 ve HTTP/2 sunuyor"]),
    ("transport_h2", ["Cap'n Proto · HTTP/2 only", "Cap'n Proto · HTTP/2 만", "Cap'n Proto · jen HTTP/2", "Cap'n Proto · HTTP/2 のみ", "Cap'n Proto · 淨係 HTTP/2", "Cap'n Proto · 仅 HTTP/2", "Cap'n Proto · solo HTTP/2", "Cap'n Proto · yalnızca HTTP/2"]),
    ("safety_open", ["local mode, no login · folders under ~ only", "로컬 모드, 로그인 없음 · ~ 아래 폴더만", "místní režim, bez přihlášení · jen složky pod ~", "ローカルモード、ログインなし · ~ 以下のフォルダのみ", "本機模式，唔使登入 · 淨係 ~ 之下嘅資料夾", "本地模式，无需登录 · 仅限 ~ 下的文件夹", "modo local, sin inicio de sesión · solo carpetas bajo ~", "yerel mod, giriş yok · yalnızca ~ altındaki klasörler"]),
    ("safety_wallet", ["wallet sign-in · folders under ~ only", "지갑 로그인 · ~ 아래 폴더만", "přihlášení peněženkou · jen složky pod ~", "ウォレットでサインイン · ~ 以下のフォルダのみ", "錢包登入 · 淨係 ~ 之下嘅資料夾", "钱包登录 · 仅限 ~ 下的文件夹", "inicio con cartera · solo carpetas bajo ~", "cüzdanla giriş · yalnızca ~ altındaki klasörler"]),
    ("safety_token", ["token required · folders under ~ only", "토큰 필요 · ~ 아래 폴더만", "vyžadován token · jen složky pod ~", "トークン必須 · ~ 以下のフォルダのみ", "要 token · 淨係 ~ 之下嘅資料夾", "需要令牌 · 仅限 ~ 下的文件夹", "token obligatorio · solo carpetas bajo ~", "belirteç gerekli · yalnızca ~ altındaki klasörler"]),
    ("waiting", ["{} waiting", "대기 {}건", "čeká: {}", "待機 {}", "{} 個等緊", "{} 个等待中", "{} en espera", "{} bekliyor"]),
    ("min", [" min", "분", " min", "分", " 分鐘", " 分钟", " min", " dk"]),
    ("remote_title", ["Remote access", "원격 접속", "Vzdálený přístup", "リモートアクセス", "遠端存取", "远程访问", "Acceso remoto", "Uzaktan erişim"]),
    ("local_mode", ["Local mode", "로컬 모드", "Místní režim", "ローカルモード", "本機模式", "本地模式", "Modo local", "Yerel mod"]),
    ("local_desc", [" · this machine only, no login. ", " · 이 컴퓨터에서만, 로그인 없음. ", " · jen tento počítač, bez přihlášení. ", " · このマシンのみ、ログイン不要。", " · 淨係呢部機，唔使登入。", " · 仅限本机，无需登录。", " · solo esta máquina, sin inicio de sesión. ", " · yalnızca bu makine, giriş yok. "]),
    ("listening", ["Listening on {}", "{} 에서 수신 중", "Naslouchá na {}", "{} で待ち受け中", "監聽緊 {}", "正在监听 {}", "Escuchando en {}", "{} üzerinde dinliyor"]),
    ("remote_explain", ["To use the backend from other devices (phone, tablet, another computer), set the wallet that owns it. The backend then listens on every interface, and you sign in with that wallet's recovery phrase.", "다른 기기(휴대폰, 태블릿, 다른 컴퓨터)에서 쓰려면 소유 지갑을 지정하세요. 그러면 백엔드가 모든 인터페이스에서 수신하고, 그 지갑의 복구 문구로 로그인합니다.", "Chcete-li backend používat z jiných zařízení (telefon, tablet, jiný počítač), nastavte peněženku, které patří. Backend pak naslouchá na všech rozhraních a přihlásíte se obnovovací frází této peněženky.", "他の端末(スマートフォン、タブレット、別のPC)から使うには、所有ウォレットを設定します。バックエンドはすべてのインターフェースで待ち受け、そのウォレットのリカバリーフレーズでサインインします。", "想喺其他裝置(手機、平板、另一部電腦)用，就設定擁有佢嘅錢包。之後後端會喺所有介面監聽，你用嗰個錢包嘅助記詞登入。", "要在其他设备(手机、平板、另一台电脑)上使用，请设置拥有它的钱包。后端随后会在所有接口上监听，你用该钱包的助记词登录。", "Para usar el backend desde otros dispositivos (teléfono, tableta, otro ordenador), indica la cartera propietaria. El backend escuchará en todas las interfaces y entrarás con la frase de recuperación de esa cartera.", "Arka ucu başka cihazlardan (telefon, tablet, başka bilgisayar) kullanmak için sahibi olan cüzdanı belirleyin. Arka uç o zaman tüm arayüzlerde dinler ve o cüzdanın kurtarma ifadesiyle giriş yaparsınız."]),
    ("remote_ph", ["wallet address 0x…  (or the recovery phrase, to work it out here)", "지갑 주소 0x…  (또는 복구 문구를 넣으면 여기서 주소를 계산)", "adresa peněženky 0x…  (nebo obnovovací fráze, adresa se spočítá zde)", "ウォレットアドレス 0x…  (またはリカバリーフレーズ。ここでアドレスを算出)", "錢包地址 0x…  (或者助記詞，喺度計出地址)", "钱包地址 0x…  (或助记词，在此算出地址)", "dirección de la cartera 0x…  (o la frase de recuperación, para calcularla aquí)", "cüzdan adresi 0x…  (veya kurtarma ifadesi; adres burada hesaplanır)"]),
    ("remote_on", ["Turn on remote mode", "원격 모드 켜기", "Zapnout vzdálený režim", "リモートモードをオン", "開啟遠端模式", "开启远程模式", "Activar modo remoto", "Uzak modu aç"]),
    ("remote_mode", ["Remote mode", "원격 모드", "Vzdálený režim", "リモートモード", "遠端模式", "远程模式", "Modo remoto", "Uzak mod"]),
    ("remote_sub", ["Listening on {} · sign-in with the wallet's recovery phrase", "{} 에서 수신 중 · 지갑 복구 문구로 로그인", "Naslouchá na {} · přihlášení obnovovací frází peněženky", "{} で待ち受け中 · ウォレットのリカバリーフレーズでサインイン", "監聽緊 {} · 用錢包助記詞登入", "正在监听 {} · 使用钱包助记词登录", "Escuchando en {} · inicio con la frase de recuperación de la cartera", "{} üzerinde dinliyor · cüzdanın kurtarma ifadesiyle giriş"]),
    ("remote_off", ["Turn off (this machine only, no login)", "끄기 (이 컴퓨터에서만, 로그인 없음)", "Vypnout (jen tento počítač, bez přihlášení)", "オフにする(このマシンのみ、ログイン不要)", "熄咗佢(淨係呢部機，唔使登入)", "关闭(仅限本机，无需登录)", "Desactivar (solo esta máquina, sin inicio de sesión)", "Kapat (yalnızca bu makine, giriş yok)"]),
    ("remote_fixed", ["Set in {}: remove CWBCODE_WALLET there and restart the backend to turn it off.", "{} 에 설정되어 있습니다. 끄려면 거기서 CWBCODE_WALLET 을 지우고 백엔드를 다시 시작하세요.", "Nastaveno v {}: pro vypnutí tam odstraňte CWBCODE_WALLET a restartujte backend.", "{} で設定されています。オフにするにはそこから CWBCODE_WALLET を削除し、バックエンドを再起動してください。", "喺 {} 度設定咗：要熄就喺嗰度刪走 CWBCODE_WALLET，再重新啟動後端。", "已在 {} 中设置：要关闭，请在那里删除 CWBCODE_WALLET 并重启后端。", "Definido en {}: elimina CWBCODE_WALLET allí y reinicia el backend para desactivarlo.", "{} içinde ayarlı: kapatmak için oradan CWBCODE_WALLET'ı kaldırıp arka ucu yeniden başlatın."]),
    ("remote_derived", ["That phrase is wallet {}. Only this address is sent and saved.", "이 문구의 지갑은 {} 입니다. 이 주소만 전송되고 저장됩니다.", "Tato fráze patří peněžence {}. Odesílá a ukládá se jen tato adresa.", "このフレーズのウォレットは {} です。送信・保存されるのはこのアドレスだけです。", "呢組助記詞係錢包 {}。淨係會送出同儲存呢個地址。", "该助记词对应钱包 {}。只会发送并保存这个地址。", "Esa frase es la cartera {}. Solo se envía y se guarda esta dirección.", "Bu ifade {} cüzdanına ait. Yalnızca bu adres gönderilir ve kaydedilir."]),
    ("remote_bad", ["Type the wallet's address (0x and 40 hex digits), or its recovery phrase to work the address out here.", "지갑 주소(0x 와 16진수 40자리)를 입력하거나, 복구 문구를 넣어 여기서 주소를 계산하세요.", "Zadejte adresu peněženky (0x a 40 hexadecimálních číslic), nebo její obnovovací frázi, z níž se adresa spočítá zde.", "ウォレットアドレス(0x と16進数40桁)を入力するか、リカバリーフレーズを入力してここでアドレスを算出してください。", "輸入錢包地址(0x 加 40 個十六進位數字)，或者輸入助記詞喺度計出地址。", "请输入钱包地址(0x 加 40 位十六进制数字)，或输入助记词在此算出地址。", "Escribe la dirección de la cartera (0x y 40 dígitos hexadecimales) o su frase de recuperación para calcularla aquí.", "Cüzdan adresini (0x ve 40 onaltılık hane) yazın ya da adresi burada hesaplamak için kurtarma ifadesini girin."]),
    ("no_projects", ["No project yet. Open a folder under your home folder from the sidebar.", "아직 프로젝트가 없습니다. 사이드바에서 홈 폴더 아래의 폴더를 여세요.", "Zatím žádný projekt. V postranním panelu otevřete složku ve svém domovském adresáři.", "プロジェクトはまだありません。サイドバーからホームフォルダ以下のフォルダを開いてください。", "仲未有專案。喺側邊欄開一個主目錄之下嘅資料夾。", "还没有项目。请在侧边栏打开主目录下的文件夹。", "Aún no hay proyectos. Abre una carpeta de tu carpeta personal desde la barra lateral.", "Henüz proje yok. Kenar çubuğundan ev klasörünüzün altındaki bir klasörü açın."]),
    ("th_project", ["Project", "프로젝트", "Projekt", "プロジェクト", "專案", "项目", "Proyecto", "Proje"]),
    ("th_folder", ["Folder", "폴더", "Složka", "フォルダ", "資料夾", "文件夹", "Carpeta", "Klasör"]),
    ("th_opened", ["Last opened", "마지막으로 연 시각", "Naposledy otevřeno", "最終オープン", "上次開啟", "上次打开", "Última apertura", "Son açılış"]),
    ("new_conversation", ["new conversation", "새 대화", "nová konverzace", "新しい会話", "新對話", "新会话", "conversación nueva", "yeni konuşma"]),
    ("conversation_n", ["conversation #{}", "대화 #{}", "konverzace č. {}", "会話 #{}", "對話 #{}", "会话 #{}", "conversación n.º {}", "konuşma #{}"]),
    ("undo", ["Undo changes", "변경 되돌리기", "Vrátit změny", "変更を元に戻す", "還原改動", "撤销更改", "Deshacer cambios", "Değişiklikleri geri al"]),
    ("summarize", ["Summarize", "요약", "Shrnout", "要約", "總結", "总结", "Resumir", "Özetle"]),
    ("empty_open_title", ["Open a project", "프로젝트 열기", "Otevřete projekt", "プロジェクトを開く", "開一個專案", "打开一个项目", "Abre un proyecto", "Bir proje açın"]),
    ("empty_open", ["Type a folder under your home folder in the sidebar (for example ~/code/myapp) and press Open. A folder that does not exist is created.", "사이드바에 홈 폴더 아래의 폴더(예: ~/code/myapp)를 입력하고 열기를 누르세요. 없는 폴더는 새로 만듭니다.", "Do postranního panelu napište složku ve svém domovském adresáři (např. ~/code/myapp) a stiskněte Otevřít. Neexistující složka se vytvoří.", "サイドバーにホームフォルダ以下のフォルダ(例: ~/code/myapp)を入力し、「開く」を押してください。存在しないフォルダは作成されます。", "喺側邊欄輸入主目錄之下嘅資料夾(例如 ~/code/myapp)，再撳「開啟」。未有嘅資料夾會自動建立。", "在侧边栏输入主目录下的文件夹(例如 ~/code/myapp)，然后点击“打开”。不存在的文件夹会被创建。", "Escribe en la barra lateral una carpeta de tu carpeta personal (por ejemplo ~/code/myapp) y pulsa Abrir. Si no existe, se crea.", "Kenar çubuğuna ev klasörünüzün altındaki bir klasörü yazın (örneğin ~/code/myapp) ve Aç'a basın. Olmayan klasör oluşturulur."]),
    ("empty_chat_title", ["What shall we work on?", "무엇을 해 볼까요?", "Na čem budeme pracovat?", "何に取り組みましょうか?", "今日想做啲乜?", "我们来做点什么?", "¿En qué trabajamos?", "Ne üzerinde çalışalım?"]),
    ("draft_ph", ["Ask, or describe what to change ...  (Enter sends, Shift+Enter for a new line)", "질문하거나 바꿀 내용을 적으세요 ...  (Enter 전송, Shift+Enter 줄바꿈)", "Zeptejte se nebo popište, co změnit ...  (Enter odešle, Shift+Enter nový řádek)", "質問や変更内容を入力 ...  (Enterで送信、Shift+Enterで改行)", "問嘢，或者講低想改乜 ...  (Enter 送出，Shift+Enter 換行)", "提问，或描述要修改的内容 ...  (Enter 发送，Shift+Enter 换行)", "Pregunta o describe qué cambiar ...  (Enter envía, Mayús+Enter salto de línea)", "Sorun ya da neyin değişeceğini anlatın ...  (Enter gönderir, Shift+Enter yeni satır)"]),
    ("image", ["+ image", "+ 이미지", "+ obrázek", "+ 画像", "+ 圖片", "+ 图片", "+ imagen", "+ görsel"]),
    ("mode_ask", ["ask first", "먼저 묻기", "nejdřív se zeptat", "先に確認", "先問我", "先询问", "preguntar antes", "önce sor"]),
    ("mode_auto", ["auto", "자동", "automaticky", "自動", "自動", "自动", "automático", "otomatik"]),
    ("mode_plan", ["plan first", "계획 먼저", "nejdřív plán", "まず計画", "先出計劃", "先做计划", "planear primero", "önce plan"]),
    ("mode_ro", ["read-only", "읽기 전용", "jen pro čtení", "読み取り専用", "唯讀", "只读", "solo lectura", "salt okunur"]),
    ("think_default", ["thinking: default", "생각: 기본", "přemýšlení: výchozí", "思考: 既定", "思考: 預設", "思考: 默认", "razonamiento: predeterminado", "düşünme: varsayılan"]),
    ("send", ["Send", "보내기", "Odeslat", "送信", "送出", "发送", "Enviar", "Gönder"]),
    ("stop", ["Stop", "중지", "Zastavit", "停止", "停止", "停止", "Detener", "Durdur"]),
    ("allow", ["Allow", "허용", "Povolit", "許可", "允許", "允许", "Permitir", "İzin ver"]),
    ("always", ["Always this session", "이번 세션 동안 항상", "Vždy v této relaci", "このセッション中は常に", "今次都允許", "本次会话始终允许", "Siempre en esta sesión", "Bu oturumda her zaman"]),
    ("no", ["No", "아니오", "Ne", "いいえ", "唔好", "否", "No", "Hayır"]),
    ("instead_ph", ["or say what to do instead, then Enter", "또는 대신 할 일을 적고 Enter", "nebo napište, co udělat místo toho, a Enter", "または代わりにすることを入力して Enter", "或者講低想點做，再撳 Enter", "或输入替代做法，然后按 Enter", "o di qué hacer en su lugar y pulsa Enter", "ya da bunun yerine ne yapılacağını yazıp Enter'a basın"]),
    ("answered", ["answered: {}", "응답: {}", "odpověď: {}", "回答: {}", "已回答: {}", "已回答: {}", "respuesta: {}", "yanıt: {}"]),
    ("running", ["running ...", "실행 중 ...", "běží ...", "実行中 ...", "行緊 ...", "运行中 ...", "ejecutando ...", "çalışıyor ..."]),
    ("kb_chip", ["BM25 + semantic", "BM25 + 의미 검색", "BM25 + sémantika", "BM25 + セマンティック", "BM25 + 語意", "BM25 + 语义", "BM25 + semántica", "BM25 + anlamsal"]),
    ("working_dots", ["working ...", "작업 중 ...", "pracuji ...", "処理中 ...", "做緊 ...", "处理中 ...", "trabajando ...", "çalışıyor ..."]),
    ("kb_search_ph", ["Search by keywords and meaning ...", "키워드와 의미로 검색 ...", "Hledat podle klíčových slov a významu ...", "キーワードと意味で検索 ...", "用關鍵字同意思搜尋 ...", "按关键词和语义搜索 ...", "Buscar por palabras clave y significado ...", "Anahtar kelime ve anlama göre ara ..."]),
    ("search", ["Search", "검색", "Hledat", "検索", "搜尋", "搜索", "Buscar", "Ara"]),
    ("no_matches", ["No matches.", "결과가 없습니다.", "Nic nenalezeno.", "一致なし。", "搵唔到。", "没有匹配。", "Sin resultados.", "Eşleşme yok."]),
    ("kb_save_title", ["Save a note or a file", "메모나 파일 저장", "Uložit poznámku nebo soubor", "メモやファイルを保存", "儲存筆記或者檔案", "保存笔记或文件", "Guardar una nota o un archivo", "Not veya dosya kaydet"]),
    ("kb_title_ph", ["Title (optional)", "제목 (선택)", "Název (nepovinný)", "タイトル(任意)", "標題(可以唔填)", "标题(可选)", "Título (opcional)", "Başlık (isteğe bağlı)"]),
    ("kb_note_ph", ["Something to remember ...", "기억해 둘 내용 ...", "Něco k zapamatování ...", "覚えておきたいこと ...", "想記低嘅嘢 ...", "想记住的内容 ...", "Algo que recordar ...", "Hatırlanacak bir şey ..."]),
    ("save_note", ["Save note", "메모 저장", "Uložit poznámku", "メモを保存", "儲存筆記", "保存笔记", "Guardar nota", "Notu kaydet"]),
    ("upload", ["Upload files (text, images, anything)", "파일 올리기 (텍스트, 이미지, 무엇이든)", "Nahrát soubory (text, obrázky, cokoli)", "ファイルをアップロード(テキスト、画像など)", "上載檔案(文字、圖片、乜都得)", "上传文件(文本、图片等)", "Subir archivos (texto, imágenes, lo que sea)", "Dosya yükle (metin, görsel, her şey)"]),
    ("delete", ["Delete", "삭제", "Smazat", "削除", "刪除", "删除", "Eliminar", "Sil"]),
    ("close", ["Close", "닫기", "Zavřít", "閉じる", "關閉", "关闭", "Cerrar", "Kapat"]),
    ("entries", ["{} entries", "항목 {}개", "položek: {}", "{} 件", "{} 項", "{} 条", "{} entradas", "{} kayıt"]),
    ("tools_sub", ["written by the agent in Rust, compiled, kept for every project", "에이전트가 Rust 로 작성하고 컴파일해 모든 프로젝트에서 사용", "napsané agentem v Rustu, zkompilované, dostupné ve všech projektech", "エージェントが Rust で書き、コンパイルし、全プロジェクトで利用", "由助手用 Rust 寫、編譯好，所有專案都用得", "由助手用 Rust 编写并编译，所有项目可用", "escritas por el agente en Rust, compiladas y disponibles en todos los proyectos", "ajan tarafından Rust ile yazıldı, derlendi, her projede kullanılabilir"]),
    ("no_tools_title", ["No tools yet", "아직 도구가 없습니다", "Zatím žádné nástroje", "ツールはまだありません", "仲未有工具", "还没有工具", "Aún no hay herramientas", "Henüz araç yok"]),
    ("no_tools", ["Ask the agent for something repetitive, or say \"make a tool that ...\".", "반복적인 작업을 시키거나 \"…하는 도구를 만들어\" 라고 말해 보세요.", "Zadejte agentovi něco opakujícího se, nebo řekněte „vytvoř nástroj, který …“.", "繰り返しの作業を頼むか、「…するツールを作って」と言ってみてください。", "叫助手做啲重複嘅嘢，或者話「整個工具嚟…」。", "让助手做些重复性的事，或者说“做一个…的工具”。", "Pide al agente algo repetitivo, o di «haz una herramienta que …».", "Ajandan tekrarlayan bir iş isteyin ya da \"… yapan bir araç yap\" deyin."]),
    ("args_source", ["arguments and source", "인자와 소스", "argumenty a zdrojový kód", "引数とソース", "參數同原始碼", "参数与源码", "argumentos y código fuente", "argümanlar ve kaynak"]),
    ("theme", ["Light / dark", "밝게 / 어둡게", "Světlý / tmavý", "ライト / ダーク", "淺色 / 深色", "浅色 / 深色", "Claro / oscuro", "Açık / koyu"]),
    ("layout", ["Side by side / stacked", "가로 / 세로 배치", "Vedle sebe / pod sebou", "横並び / 縦並び", "橫排 / 直排", "横向 / 纵向", "En paralelo / apilado", "Yan yana / alt alta"]),
    ("language", ["Language", "언어", "Jazyk", "言語", "語言", "语言", "Idioma", "Dil"]),
    ("run", ["Run", "실행", "Spustit", "実行", "執行", "运行", "Ejecutar", "Çalıştır"]),
    ("running", ["Running ...", "실행 중 ...", "Běží ...", "実行中 ...", "執行緊 ...", "运行中 ...", "Ejecutando ...", "Çalışıyor ..."]),
    ("console", ["Console", "콘솔", "Konzole", "コンソール", "主控台", "控制台", "Consola", "Konsol"]),
    ("clear", ["Clear", "지우기", "Vymazat", "クリア", "清除", "清除", "Limpiar", "Temizle"]),
    ("console_ph", ["command to run (empty: guessed from the folder)", "실행할 명령 (비우면 폴더에서 추측)", "příkaz ke spuštění (prázdné: odhadne se ze složky)", "実行するコマンド（空: フォルダから推測）", "要執行嘅指令（留空：由資料夾估）", "要运行的命令（留空：根据文件夹推测）", "comando a ejecutar (vacío: se deduce de la carpeta)", "çalıştırılacak komut (boş: klasörden tahmin edilir)"]),
    ("console_empty", ["Nothing has been run yet.", "아직 아무것도 실행하지 않았습니다.", "Zatím nic neběželo.", "まだ何も実行していません。", "仲未執行過嘢。", "还没有运行过任何东西。", "Todavía no se ha ejecutado nada.", "Henüz hiçbir şey çalıştırılmadı."]),
    ("settings", ["Settings", "설정", "Nastavení", "設定", "設定", "设置", "Ajustes", "Ayarlar"]),
    ("copy_all", ["Copy all", "전체 복사", "Kopírovat vše", "すべてコピー", "複製全部", "复制全部", "Copiar todo", "Tümünü kopyala"]),
    ("copied", ["Conversation copied", "대화를 복사했습니다", "Konverzace zkopírována", "会話をコピーしました", "已經複製對話", "已复制对话", "Conversación copiada", "Konuşma kopyalandı"]),
    ("copy_failed", ["Could not copy", "복사하지 못했습니다", "Nelze zkopírovat", "コピーできませんでした", "複製唔到", "无法复制", "No se pudo copiar", "Kopyalanamadı"]),
    ("remove_project", ["Remove from the list", "목록에서 제거", "Odebrat ze seznamu", "一覧から削除", "喺清單移除", "从列表移除", "Quitar de la lista", "Listeden kaldır"]),
    ("removed", ["{} is off the list. Its folder is untouched.", "{} 을(를) 목록에서 제거했습니다. 폴더는 그대로입니다.", "{} odebráno ze seznamu. Složka zůstala.", "{} を一覧から削除しました。フォルダはそのままです。", "{} 已經喺清單移除，資料夾冇郁過。", "{} 已从列表移除，文件夹未改动。", "{} se quitó de la lista. La carpeta sigue intacta.", "{} listeden kaldırıldı. Klasöre dokunulmadı."]),
    ("context", ["Context", "컨텍스트", "Kontext", "コンテキスト", "上下文", "上下文", "Contexto", "Bağlam"]),
    ("ctx_near", ["This conversation's context is nearly full. Start a new conversation, or summarize this one.", "이 대화의 컨텍스트가 거의 찼습니다. 새 대화를 시작하거나 이 대화를 요약하세요.", "Kontext této konverzace je téměř plný. Začněte novou konverzaci, nebo tuto shrňte.", "この会話のコンテキストがほぼ一杯です。新しい会話を始めるか、この会話を要約してください。", "呢個對話嘅上下文差唔多滿。開個新對話，或者將呢個摘要。", "此对话的上下文即将用尽。请开始新对话，或对此对话做摘要。", "El contexto de esta conversación está casi lleno. Empieza una nueva o resume esta.", "Bu konuşmanın bağlamı neredeyse doldu. Yeni bir konuşma başlatın ya da bunu özetleyin."]),
    ("ctx_full", ["The context is full: start a new conversation.", "컨텍스트가 가득 찼습니다. 새 대화를 시작하세요.", "Kontext je plný: začněte novou konverzaci.", "コンテキストが一杯です。新しい会話を始めてください。", "上下文滿咗：開個新對話。", "上下文已满：请开始新对话。", "El contexto está lleno: empieza una conversación nueva.", "Bağlam doldu: yeni bir konuşma başlatın."]),
    ("sidebar", ["Show or hide the sidebar", "사이드바 보이기 / 숨기기", "Zobrazit nebo skrýt panel", "サイドバーの表示 / 非表示", "顯示或隱藏側欄", "显示或隐藏侧栏", "Mostrar u ocultar la barra lateral", "Kenar çubuğunu göster / gizle"]),
    ("font", ["Typeface", "글꼴", "Písmo", "フォント", "字型", "字体", "Tipo de letra", "Yazı tipi"]),
    ("font_system", ["System", "시스템", "Systémové", "システム", "系統", "系统", "Sistema", "Sistem"]),
    ("font_smaller", ["Smaller messages", "글자 작게", "Menší písmo", "文字を小さく", "字細啲", "缩小文字", "Texto más pequeño", "Yazıyı küçült"]),
    ("font_larger", ["Larger messages", "글자 크게", "Větší písmo", "文字を大きく", "字大啲", "放大文字", "Texto más grande", "Yazıyı büyüt"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_text_is_there_in_every_language_with_its_blank() {
        for (key, texts) in TEXTS {
            assert_eq!(TEXTS.iter().filter(|(k, _)| k == key).count(), 1, "{key} appears twice");
            for (lang, text) in Lang::ALL.iter().zip(texts) {
                assert!(!text.trim().is_empty(), "{key} is empty in {lang:?}");
                assert_eq!(text.matches("{}").count(), texts[0].matches("{}").count(), "{key} in {lang:?}: the blank to fill in");
            }
        }
        assert_eq!(tr(Lang::Ko, "nav_chat"), "채팅");
        assert_eq!(tr_with(Lang::Es, "created", "/x"), "Se creó /x");
        assert_eq!(tr(Lang::En, "no-such-key"), "?");
        assert_eq!(Lang::from_code("yue"), Some(Lang::Yue));
        assert_eq!((Lang::from_browser("ko-KR"), Lang::from_browser("zh-HK"), Lang::from_browser("zh-CN"), Lang::from_browser("fr")), (Lang::Ko, Lang::Yue, Lang::Zh, Lang::En));
    }
}
