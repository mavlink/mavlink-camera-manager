fn main() {
    // Configure vergen
    let mut config = vergen::Config::default();
    *config.build_mut().semver_mut() = true;
    *config.build_mut().timestamp_mut() = true;
    *config.build_mut().kind_mut() = vergen::TimestampKind::DateOnly;
    *config.git_mut().sha_kind_mut() = vergen::ShaKind::Short;

    vergen::vergen(config).expect("Unable to generate the cargo keys!");
}
