//! Interpreter-only diagnostic entry; generated games call the library with their lift table.
fn main() {
    let result = (|| -> Result<(), String> {
        let mut args = std::env::args().skip(1);
        let path = args.next().ok_or("usage: mkds_runtime_probe <rom.nds> [--headless --instructions N] [--frames N] [--mute] [--capture DIR]")?;
        if path == "--assets" {
            let path = args.next().ok_or("--assets needs DIR ROM_SHA256 ASSETS_SHA256")?;
            let rom_hash = args.next().ok_or("Missing ROM_SHA256")?;
            let assets_hash = args.next().ok_or("Missing ASSETS_SHA256")?;
            let options = mkds_native::Options::from_args(args)?;
            eprintln!("Diagnostic interpreter-only assets run; no generated game table supplied.");
            mkds_native::run_from_assets(std::path::Path::new(&path), &rom_hash, &assets_hash, &[], &[], options)?;
            return Ok(());
        }
        let options = mkds_native::Options::from_args(args)?;
        let rom = std::fs::read(&path).map_err(|e| format!("Cannot read ROM {path}: {e}"))?;
        let image = mkds_builder::extract::arm9_main(&rom)
            .map_err(|e| format!("ARM9 extraction failed: {e}"))?;
        eprintln!("Diagnostic interpreter-only run; no generated game table supplied.");
        mkds_native::run(&rom, &image, &[], options)?;
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
