use std::{path::Path, process::Command};

use vergen_gix::{BuildBuilder, CargoBuilder, DependencyKind, GixBuilder};

fn file_download(url: &str, output: &str) {
    let mut resp =
        reqwest::blocking::get(url).unwrap_or_else(|_| panic!("Failed to download file: {url}"));
    let file_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(output);
    let mut output_file = std::fs::File::create(&file_path)
        .unwrap_or_else(|_| panic!("Failed to create file: {file_path:?}"));
    std::io::copy(&mut resp, &mut output_file).expect("Failed to copy content.");
}

#[cfg(windows)]
fn print_link_search_path() {
    use std::env;

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    if cfg!(target_arch = "x86_64") {
        println!("cargo:rustc-link-search=native={}/lib/x64", manifest_dir);
    } else {
        println!("cargo:rustc-link-search=native={}/lib", manifest_dir);
    }
}

#[cfg(not(windows))]
fn print_link_search_path() {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    print_link_search_path();

    generate_build_details()?;

    file_download(
        "https://unpkg.com/vue@3.0.5/dist/vue.global.js",
        "src/html/vue.js",
    );

    // set SKIP_WEB=1 to skip
    if std::env::var("SKIP_WEB").is_err() {
        build_web();
    }

    Ok(())
}

fn build_web() {
    // Note that as we are not watching all files, sometimes we'd need to force this build
    println!("cargo:rerun-if-changed=./frontend/index.html");
    println!("cargo:rerun-if-changed=./frontend/package.json");
    println!("cargo:rerun-if-changed=./frontend/vite.config.ts");
    println!("cargo:rerun-if-changed=./frontend/tsconfig.json");
    println!("cargo:rerun-if-changed=./frontend/tsconfig.config.json");
    println!("cargo:rerun-if-changed=./frontend/src");

    let frontend_dir = Path::new("./frontend");
    frontend_dir.try_exists().unwrap();

    let program = if Command::new("bun")
        .args(["--version"])
        .status()
        .ok()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        "bun"
    } else {
        "yarn"
    };

    let version = Command::new(program)
        .args(["--version"])
        .status()
        .unwrap_or_else(|_| {
            panic!("Failed to build frontend, `{program}` appears to be not installed.",)
        });

    if !version.success() {
        panic!("{program} version failed!");
    }

    let install = Command::new(program)
        .args(["install", "--frozen-lockfile"])
        .current_dir(frontend_dir)
        .status()
        .unwrap();

    if !install.success() {
        panic!("{program} install failed!");
    }

    let args = if program == "bun" {
        vec!["run", "build"]
    } else {
        vec!["build"]
    };
    let build = Command::new(program)
        .args(&args)
        .current_dir(frontend_dir)
        .status()
        .unwrap();

    if !build.success() {
        panic!("{program} build failed!");
    }
}

fn generate_build_details() -> Result<(), Box<dyn std::error::Error>> {
    let mut emitter = vergen_gix::Emitter::default();

    emitter.add_instructions(&BuildBuilder::all_build()?)?;
    emitter.add_instructions(
        CargoBuilder::all_cargo()?.set_dep_kind_filter(Some(DependencyKind::Normal)),
    )?;

    if std::path::Path::new(".git").is_dir() {
        emitter.add_instructions(&GixBuilder::all_git()?)?;
    }

    emitter.emit()?;

    Ok(())
}
