// The field catalog remains the single source used by the server and Mac editor.
#[path = "../hysteriax-server/src/api/subscriptions/templates/schema_transform.rs"]
mod schema_transform;

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets manifest dir"))
        .join("../hysteriax-server/src/api/subscriptions/templates");
    println!(
        "cargo:rerun-if-changed={}",
        source.join("schema_transform.rs").display()
    );
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let mut generated = String::new();
    for (format, name) in [("mihomo", "Mihomo"), ("singbox", "Singbox")] {
        let path = source.join(format!("{format}.schema.json"));
        println!("cargo:rerun-if-changed={}", path.display());
        let raw: serde_json::Value = serde_json::from_slice(
            &fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        jsonschema::meta::validate(&raw)
            .unwrap_or_else(|error| panic!("invalid raw {format} schema: {error}"));
        let prepared = schema_transform::prepare(raw);
        jsonschema::meta::validate(&prepared)
            .unwrap_or_else(|error| panic!("invalid prepared {format} schema: {error}"));
        let artifact = out.join(format!("{format}.validation.json"));
        fs::write(
            &artifact,
            serde_json::to_vec(&prepared).expect("serialize checked schema"),
        )
        .expect("write checked schema artifact");
        generated.push_str(&format!(
            "#[jsonschema::validator(path = {:?})]\npub(super) struct {name};\n",
            artifact.to_str().expect("UTF-8 Cargo output path")
        ));
    }
    fs::write(out.join("validators.rs"), generated).expect("write validator declarations");
}
