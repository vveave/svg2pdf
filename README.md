# svg2pdf

[![Build status](https://github.com/vveave/svg2pdf/actions/workflows/ci.yml/badge.svg)](https://github.com/vveave/svg2pdf/actions/workflows/ci.yml)

Convert SVG files to PDFs.

This crate allows to convert static (i.e. non-interactive) SVG files to
either standalone PDF files or Form XObjects that can be embedded in another
PDF file and used just like images.

Generate API documentation locally with `cargo doc --workspace --no-deps`.

## CLI

This crate also contains a command line interface. Install it by running the
command below:

```bash
cargo install --git https://github.com/vveave/svg2pdf.git svg2pdf-cli
```

You can then convert SVGs to PDFs by running commands like these:

```bash
svg2pdf your.svg
```

## Contributing
We are looking forward to receiving your bugs and feature requests in the Issues
tab. We would also be very happy to accept PRs for bug fixes, features, or
refactorings! We'd be happy to assist you
with your PR's, so feel free to post Work in Progress PRs if marked as such.
Please be kind to the maintainers and other contributors. If you feel that there
are any problems, please feel free to reach out to us privately.

Thanks to each and every prospective contributor for the effort you (plan to)
invest in this project and for adopting it!

## License
`svg2pdf` is licensed under a MIT / Apache 2.0 dual license.

Users and consumers of the library may choose which of those licenses they want
to apply whereas contributors have to accept that their code is in compliance
and distributed under the terms of both of these licenses.
