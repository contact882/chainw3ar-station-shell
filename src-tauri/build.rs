// Build gate: the pairing between this shell and the frozen UI contract is
// enforced at compile time. A drifted vendored contract or a missing init
// script bundle is a build ERROR, not a runtime surprise.
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let vendor = manifest.join("../bridge/vendor");

    println!("cargo:rerun-if-changed=../bridge/vendor/contract.ts");
    println!("cargo:rerun-if-changed=../bridge/vendor/conformance.ts");
    println!("cargo:rerun-if-changed=../bridge/vendor/PIN.json");
    println!("cargo:rerun-if-changed=gen/init_script.js");

    let pin: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(vendor.join("PIN.json")).expect("read PIN.json"))
            .expect("parse PIN.json");
    for (file, expected) in pin["sha256"].as_object().expect("sha256 map") {
        let raw = fs::read_to_string(vendor.join(file))
            .unwrap_or_else(|e| panic!("read vendored {file}: {e}"));
        let normalized = raw.replace("\r\n", "\n");
        let actual = hex::encode(Sha256::digest(normalized.as_bytes()));
        let expected = expected.as_str().expect("hash string");
        assert_eq!(
            actual, expected,
            "\n\nCONTRACT PIN VIOLATION: bridge/vendor/{file} does not match the frozen \
             {} blob.\nThe contract is frozen; never edit the vendored copies.\n",
            pin["tag"]
        );
    }

    assert!(
        manifest.join("gen/init_script.js").exists(),
        "src-tauri/gen/init_script.js missing — run `npm run build:web` first"
    );

    tauri_build::build();
}

// Tiny local hex encoder — not worth a dependency in a build script.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}
