use super::{herdr_layer_to_starter, merged_with_budget, rewrite_retired_distro_layer};
use std::path::Path;

#[test]
fn a_missing_herdr_layer_loads_the_starter_beside_it() {
    assert_eq!(
        herdr_layer_to_starter(Path::new("/tmp/not-a-distro/herdr.toml")),
        None
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let distros = dir.path().join("distros");
    std::fs::create_dir_all(distros.join("starter")).expect("starter dir");
    std::fs::create_dir_all(distros.join("herdr")).expect("herdr dir");
    let starter = distros.join("starter").join("starter.toml");
    std::fs::write(&starter, "defaults.history-limit = 12345\n").expect("starter.toml");
    let herdr = distros.join("herdr").join("herdr.toml");

    let user = dir.path().join("config.toml");
    let input = format!("extends = [\"{}\"]\n", herdr.display());
    let (merged, _) =
        merged_with_budget("", &input, &user, None).expect("retired path still loads");
    assert_eq!(
        merged["defaults"]["history-limit"].as_integer(),
        Some(12345)
    );

    // A still-present herdr stub wins unchanged.
    std::fs::write(&herdr, "defaults.history-limit = 7\n").expect("stub");
    assert_eq!(rewrite_retired_distro_layer(&herdr), herdr);
}
