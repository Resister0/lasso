fn main() {
    // One .slint file exports every window (overlay, popups, pin, settings, library).
    // Default style "fluent" is never used (own widgets), but slint-build needs one.
    let cfg = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/main.slint", cfg).expect("slint build");
    windows_resources();
}

/// Windows: Lasso's icon, version info (Explorer, Properties > Details - an exe without them looks more suspicious to
/// antivirus heuristics) and a manifest: per-monitor DPI v2 from the first instruction (the frozen screen is in
/// physical pixels), common controls v6 (modern Save as / Open dialogs), Windows 10/11, no admin rights.
/// Same way as Echo's build.rs. CI sets LASSO_VERSION.
fn windows_resources() {
    println!("cargo:rerun-if-env-changed=LASSO_VERSION");
    println!("cargo:rerun-if-env-changed=LASSO_RELEASE");
    println!("cargo:rerun-if-env-changed=LASSO_UPDATE_PUBKEY");
    println!("cargo:rerun-if-changed=assets/icons/lasso.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ver = std::env::var("LASSO_VERSION").unwrap_or_else(|_| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    let n: Vec<u64> = ver.split(['.', '-', '+']).take(3).map(|x| x.parse().unwrap_or(0)).collect();
    let packed = (n.first().copied().unwrap_or(0) << 48) | (n.get(1).copied().unwrap_or(0) << 32) | (n.get(2).copied().unwrap_or(0) << 16);
    let mut r = winresource::WindowsResource::new();
    r.set_icon("assets/icons/lasso.ico")
        .set("ProductName", "Sonela Lasso")
        .set("FileDescription", "Sonela Lasso")
        .set("CompanyName", "Sonela")
        .set("LegalCopyright", "© 2026 Sonela")
        .set("OriginalFilename", "lasso.exe")
        .set("InternalName", "lasso")
        .set("FileVersion", &ver)
        .set("ProductVersion", &ver)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed)
        .set_manifest(MANIFEST);
    if let Err(e) = r.compile() {
        // No resource compiler: still build, just without icon / details / manifest (the code sets the DPI mode itself).
        // Only worth a warning on a Windows host - the Linux type-check (no mingw windres) always lands here.
        if cfg!(windows) {
            println!("cargo:warning=exe icon / version info / manifest skipped: {e}");
        }
    }
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Resister0.Lasso" version="1.0.0.0" processorArchitecture="*"/>
  <description>Sonela Lasso</description>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;
