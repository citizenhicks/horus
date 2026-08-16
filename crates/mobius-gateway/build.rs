use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let resource = manifest_dir.join("src/defaults.toml");
    println!("cargo:rerun-if-changed={}", resource.display());

    let source = fs::read_to_string(&resource)
        .unwrap_or_else(|error| panic!("read {}: {error}", resource.display()));
    let table = source
        .parse::<toml::Table>()
        .unwrap_or_else(|error| panic!("parse {}: {error}", resource.display()));
    for key in table.keys() {
        if !matches!(key.as_str(), "system_prompt" | "context_window") {
            panic!("{} contains unknown key `{key}`", resource.display());
        }
    }
    let prompt = table
        .get("system_prompt")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{} must define system_prompt", resource.display()));
    let context_window = table
        .get("context_window")
        .and_then(toml::Value::as_integer)
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            panic!(
                "{} must define a positive context_window",
                resource.display()
            )
        });
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("out dir")).join("defaults.rs");
    fs::write(
        output,
        format!(
            "pub const DEFAULT_SYSTEM_PROMPT: &str = {prompt:?};\n\
             pub const DEFAULT_CONTEXT_WINDOW: i64 = {context_window};\n"
        ),
    )
    .expect("write generated defaults");
}
