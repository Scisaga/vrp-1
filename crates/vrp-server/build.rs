use std::{env, fs, path::Path};
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/vendor/basemap");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let mut generated = String::from("pub static ASSETS: &[(&str,&[u8])] = &[\n");
    for (name, path) in files {
        println!("cargo:rerun-if-changed={path}");
        generated.push_str(&format!("({name:?},include_bytes!({path:?})),\n"));
    }
    generated.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("basemap_assets.rs"),
        generated,
    )
    .unwrap();
    println!("cargo:rerun-if-changed={}", root.display());
}
fn collect(root: &Path, at: &Path, files: &mut Vec<(String, String)>) {
    for item in fs::read_dir(at).unwrap() {
        let path = item.unwrap().path();
        if path.is_dir() {
            collect(root, &path, files)
        } else {
            files.push((
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                path.canonicalize().unwrap().to_string_lossy().into(),
            ));
        }
    }
}
