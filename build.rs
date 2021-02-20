extern crate vergen;

use vergen::ConstantsFlags;

fn main() {
    // Generate flags about build information
    let flags = ConstantsFlags::BUILD_TIMESTAMP
        | ConstantsFlags::SEMVER
        | ConstantsFlags::BUILD_DATE
        | ConstantsFlags::SHA_SHORT;
    vergen::gen(flags).expect("Unable to generate the cargo keys!");
}
