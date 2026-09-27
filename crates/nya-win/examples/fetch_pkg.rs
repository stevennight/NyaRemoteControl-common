//! Manual check of the pinned download path: `cargo run -p nya-win --example fetch_pkg`.
fn main() -> anyhow::Result<()> {
    let pkg = nya_win::package::Package {
        file: "VirtualDisplayDriver-x86.Driver.Only.zip",
        url: "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip",
        sha256: "e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a",
    };
    let p = nya_win::package::obtain(&pkg, &mut |d, t| eprint!("\r{d}/{t:?}"))?;
    println!("\nok {}", p.display());
    let dir = nya_win::package::cache_dir().join("vdd-test");
    nya_win::package::unzip(&p, &dir)?;
    println!("unzipped: {:?}", std::fs::read_dir(dir.join("VirtualDisplayDriver"))?.flatten().map(|e| e.file_name()).collect::<Vec<_>>());
    for id in [r"ROOT\BasicRender", r"Root\MttVDD", "VBAudioVACWDM"] {
        println!("exists({id}) = {}", nya_win::devnode::exists(id));
    }
    Ok(())
}
