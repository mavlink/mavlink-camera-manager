extern crate reqwest;
extern crate vergen;

use vergen::ConstantsFlags;

fn main() {
    // Generate flags about build information
    let flags = ConstantsFlags::BUILD_TIMESTAMP
        | ConstantsFlags::SEMVER
        | ConstantsFlags::BUILD_DATE
        | ConstantsFlags::SHA_SHORT;
    vergen::gen(flags).expect("Unable to generate the cargo keys!");

    // Download vue file
    let vue_url = "https://unpkg.com/vue@3.0.5/dist/vue.global.js";
    let mut resp = reqwest::blocking::get(vue_url)
        .expect(&format!("Failed to download vue file: {}", &vue_url));
    let vue_file_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/html/vue.js");
    let mut output_file = std::fs::File::create(&vue_file_path)
        .expect(&format!("Failed to create vue file: {:?}", &vue_file_path));
    std::io::copy(&mut resp, &mut output_file).expect("Failed to copy content.");
}
