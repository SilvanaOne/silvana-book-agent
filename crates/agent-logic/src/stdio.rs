//! `out!`/`errln!`: `println!`/`eprintln!` that ignore write errors, so a closed
//! stdout or stderr cannot panic the process.

use std::fmt;
use std::io::Write;

/// Write one line to `w`, ignoring any error.
pub fn write_line<W: Write + ?Sized>(w: &mut W, args: fmt::Arguments<'_>) {
    let _ = writeln!(w, "{args}");
}

/// Like `println!`, but a failed write is ignored instead of panicking.
#[macro_export]
macro_rules! out {
    () => {
        $crate::stdio::write_line(&mut ::std::io::stdout().lock(), ::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::stdio::write_line(&mut ::std::io::stdout().lock(), ::std::format_args!($($arg)*))
    };
}

/// Like `eprintln!`, but a failed write is ignored instead of panicking.
#[macro_export]
macro_rules! errln {
    () => {
        $crate::stdio::write_line(&mut ::std::io::stderr().lock(), ::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::stdio::write_line(&mut ::std::io::stderr().lock(), ::std::format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    // A pipe whose reader is gone fails every write with EPIPE; that must not panic
    #[test]
    fn a_closed_pipe_does_not_panic() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        drop(reader);
        for i in 0..3 {
            write_line(&mut writer, format_args!("line {i}"));
        }
        write_line(&mut Broken, format_args!("{}", "ignored"));
    }

    #[test]
    fn lines_are_formatted_with_a_newline() {
        let mut buf = Vec::new();
        write_line(&mut buf, format_args!("{} = {:.2}", "x", 1.5f64));
        write_line(&mut buf, format_args!(""));
        assert_eq!(String::from_utf8(buf).unwrap(), "x = 1.50\n\n");
    }

    // Type-checks every macro form without writing to the test's terminal
    #[test]
    fn macros_expand_for_every_form() {
        let named = 3;
        let _ = || {
            crate::out!();
            crate::out!("plain");
            crate::out!("{} {}", 1, "two");
            crate::out!("{named}");
            crate::errln!();
            crate::errln!("err {}", named);
        };
    }
}
