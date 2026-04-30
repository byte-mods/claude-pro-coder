/// Backstop for any future stub subcommand. Currently unused (all subcommands
/// now have real implementations); retained so adding a stub is cheap.
#[allow(dead_code)]
pub fn not_yet(subcommand: &str, section: u32) -> Result<(), u8> {
    eprintln!(
        "lens {subcommand}: not yet implemented (section {section}). \
         Run `lens --help` for status of available subcommands."
    );
    Err(2)
}
