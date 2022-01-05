extern crate reqwest;
extern crate vergen;

fn main() {
    // Configure vergen
    let mut config = vergen::Config::default();
    *config.build_mut().semver_mut() = true;
    *config.build_mut().timestamp_mut() = true;
    *config.build_mut().kind_mut() = vergen::TimestampKind::DateOnly;
    *config.git_mut().sha_kind_mut() = vergen::ShaKind::Short;

    vergen::vergen(config).expect("Unable to generate the cargo keys!");

    // Download vue file
    let vue_url = "https://unpkg.com/vue@3.0.5/dist/vue.global.js";
    let mut resp = reqwest::blocking::get(vue_url)
        .expect(&format!("Failed to download vue file: {}", &vue_url));
    let vue_file_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/html/vue.js");
    let mut output_file = std::fs::File::create(&vue_file_path)
        .expect(&format!("Failed to create vue file: {:?}", &vue_file_path));
    std::io::copy(&mut resp, &mut output_file).expect("Failed to copy content.");
}
