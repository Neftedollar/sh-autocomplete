use shac_ml_train::personas;
use std::path::PathBuf;

#[test]
fn loads_committed_personas_file() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ml/data/personas.toml");
    let personas = personas::load(&path).expect("load personas");
    assert!(
        personas.len() >= 6,
        "expected at least 6 personas, got {}",
        personas.len()
    );
    assert!(personas.iter().any(|p| p.os == "darwin"));
    assert!(personas.iter().any(|p| p.os == "linux"));
}
