use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("Cargo must provide the manifest directory");
    let out_dir = env::var("OUT_DIR").expect("Cargo must provide the output directory");
    let icon_path = Path::new(&manifest_dir).join("src/icons/icon.ico");
    let app_manifest_path = Path::new(&manifest_dir).join("src/app.manifest");
    let rc_file = Path::new(&out_dir).join("agent-limit-monitor.rc");
    let resource_file = Path::new(&out_dir).join("agent-limit-monitor.res");
    let (major, minor, patch) = version_parts(version);

    let rc_source = format!(
        r#"#pragma code_page(65001)
1 VERSIONINFO
FILEVERSION {major}, {minor}, {patch}, 0
PRODUCTVERSION {major}, {minor}, {patch}, 0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
{{
BLOCK "StringFileInfo"
{{
BLOCK "000004b0"
{{
VALUE "CompanyName", "Sergey Stepanyuk"
VALUE "FileDescription", "Agent Limit Monitor"
VALUE "FileVersion", "{version}"
VALUE "InternalName", "AgentLimitMonitor"
VALUE "OriginalFilename", "agent-limit-monitor.exe"
VALUE "ProductName", "Agent Limit Monitor"
VALUE "ProductVersion", "{version}"
VALUE "LegalCopyright", "Copyright (C) 2026 Sergey Stepanyuk"
VALUE "Comments", "https://github.com/sstpnk/Claude-Code-Usage-Monitor"
}}
}}
BLOCK "VarFileInfo" {{
VALUE "Translation", 0x0, 0x04b0
}}
}}
1 ICON "{}"
1 24 "{}"
"#,
        rc_path(&icon_path),
        rc_path(&app_manifest_path),
    );
    fs::write(&rc_file, rc_source).expect("unable to write the Windows resource script");

    if let Some(prebuilt_resource) = env::var_os("ALM_PREBUILT_RESOURCE") {
        fs::copy(prebuilt_resource, &resource_file)
            .expect("unable to copy the prebuilt Windows resource");
    } else {
        let rc_exe = env::var_os("RC_PATH").unwrap_or_else(|| "rc.exe".into());
        let status = Command::new(rc_exe)
            .current_dir(&manifest_dir)
            .arg(format!("/I{manifest_dir}"))
            .arg(format!("/fo{}", resource_file.display()))
            .arg(&rc_file)
            .status()
            .expect("unable to run the Windows resource compiler");
        if !status.success() {
            panic!("Windows resource compiler failed with {status}");
        }
    }

    println!("cargo:rustc-link-arg={}", resource_file.display());
    println!("cargo:rerun-if-changed=src/app.manifest");
    println!("cargo:rerun-if-changed=src/icons/icon.ico");
    println!("cargo:rerun-if-env-changed=ALM_PREBUILT_RESOURCE");
}

fn version_parts(version: &str) -> (u16, u16, u16) {
    let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn rc_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
