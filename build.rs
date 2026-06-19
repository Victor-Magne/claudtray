fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/claudtray.ico");
        res.set("ProductName", "ClaudTray");
        res.set("FileDescription", "ClaudTray — AI Usage Monitor");
        res.set("LegalCopyright", "Victor Gomes");
        // 1.1.2.0 encoded as 0xMMMM_mmmm_pppp_bbbb
        res.set_version_info(winres::VersionInfo::PRODUCTVERSION, 0x0001_0001_0002_0000);
        res.set_version_info(winres::VersionInfo::FILEVERSION, 0x0001_0001_0002_0000);
        if let Err(e) = res.compile() {
            eprintln!("winres error: {e}");
        }
    }
}
