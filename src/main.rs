use runlet::{Diagnostic, Runtime, Severity};
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

fn run() -> Result<(), u8> {
    let mut args = env::args_os();
    let executable = args.next().unwrap_or_default();
    let Some(path) = args.next() else {
        eprintln!("usage: {} <program.rnlt>", executable.to_string_lossy());
        return Err(2);
    };
    if args.next().is_some() {
        eprintln!("error: expected exactly one .rnlt file");
        eprintln!("usage: {} <program.rnlt>", executable.to_string_lossy());
        return Err(2);
    }

    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("error: cannot read `{}`: {error}", path.to_string_lossy());
            return Err(2);
        }
    };

    let runtime = Runtime::builder().build().expect("core runtime is valid");
    let compiled = match runtime.compile(&source) {
        Ok(compiled) => compiled,
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                render_diagnostic(&path.to_string_lossy(), &source, &diagnostic);
            }
            return Err(1);
        }
    };

    for diagnostic in &compiled.diagnostics {
        render_diagnostic(&path.to_string_lossy(), &source, diagnostic);
    }

    let execution = match runtime.run(&compiled) {
        Ok(execution) => execution,
        Err(error) => {
            eprintln!("{}: runtime error: {error}", path.to_string_lossy());
            return Err(1);
        }
    };
    match execution.value.presentation_json() {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("{}: cannot display result: {error}", path.to_string_lossy());
            return Err(1);
        }
    }
    Ok(())
}

fn render_diagnostic(path: &str, source: &str, diagnostic: &Diagnostic) {
    let (line, column, line_text) = source_location(source, diagnostic.primary_span.start);
    let level = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    eprintln!(
        "{path}:{line}:{column}: {level}[{}]: {}",
        diagnostic.code, diagnostic.title
    );
    eprintln!("  {line_text}");
    let width = diagnostic
        .primary_span
        .end
        .saturating_sub(diagnostic.primary_span.start)
        .max(1);
    eprintln!("  {}{}", " ".repeat(column - 1), "^".repeat(width));
    eprintln!("  {}", diagnostic.message);
    if let Some(fix) = diagnostic.fixes.first() {
        eprintln!("  help: {}", fix.message);
    } else if !diagnostic.candidates.is_empty() {
        eprintln!("  candidates: {}", diagnostic.candidates.join(", "));
    }
}

fn source_location(source: &str, offset: usize) -> (usize, usize, &str) {
    let safe_offset = offset.min(source.len());
    let line_start = source[..safe_offset].rfind('\n').map_or(0, |at| at + 1);
    let line_end = source[safe_offset..]
        .find('\n')
        .map_or(source.len(), |relative| safe_offset + relative);
    let line = source[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let column = source[line_start..safe_offset].chars().count() + 1;
    (line, column, &source[line_start..line_end])
}

#[cfg(test)]
mod tests {
    use super::source_location;

    #[test]
    fn reports_unicode_columns() {
        assert_eq!(source_location("éx\nnext", 2), (1, 2, "éx"));
        assert_eq!(source_location("éx\nnext", 4), (2, 1, "next"));
    }
}
