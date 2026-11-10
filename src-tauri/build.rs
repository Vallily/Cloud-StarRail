fn main() {
    #[cfg(windows)]
    {
        cc::Build::new()
            .cpp(true)
            .file("src/xbox_share.cpp")
            .flag_if_supported("/std:c++17")
            .compile("xbox_share");
        println!("cargo:rerun-if-changed=src/xbox_share.cpp");
    }
    tauri_build::build();
}
