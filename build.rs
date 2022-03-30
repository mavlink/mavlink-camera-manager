extern crate reqwest;
extern crate vergen;

fn file_download(url: &str, output: &str) {
    let mut resp = reqwest::blocking::get(url).expect(&format!("Failed to download file: {url}"));
    let file_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(output);
    let mut output_file =
        std::fs::File::create(&file_path).expect(&format!("Failed to create file: {file_path:?}"));
    std::io::copy(&mut resp, &mut output_file).expect("Failed to copy content.");
}

fn main() {
    // Configure vergen
    let mut config = vergen::Config::default();
    *config.build_mut().semver_mut() = true;
    *config.build_mut().timestamp_mut() = true;
    *config.build_mut().kind_mut() = vergen::TimestampKind::DateOnly;
    *config.git_mut().sha_kind_mut() = vergen::ShaKind::Short;

    vergen::vergen(config).expect("Unable to generate the cargo keys!");

    file_download(
        "https://unpkg.com/vue@3.0.5/dist/vue.global.js",
        "src/html/vue.js",
    );
    std::fs::create_dir_all("src/html/webrtc/adapter").unwrap();
    file_download(
        "https://webrtc.github.io/adapter/adapter-latest.js",
        "src/html/webrtc/adapter/adapter-latest.js",
    );
    file_download(
        "https://raw.githubusercontent.com/webrtcHacks/adapter/18a8b4127cbc1376320cac5742d817b5b7dd0085/LICENSE.md",
        "src/html/webrtc/adapter/LICENSE.md",
    );
}
