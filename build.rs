fn main() {
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");

    let build_number = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "development".to_owned());

    println!("cargo:rustc-env=ADB_BUILD_NUMBER={build_number}");
}
