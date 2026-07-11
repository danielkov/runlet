# Tree-sitter grammar for Runlet

`grammar.js` is the source of truth for the Runlet grammar. It is maintained
under `editors/tree-sitter-runlet` on the repository's `main` branch.

This branch contains the generated files under `src/` so consumers such as Zed
can compile `src/parser.c` without running the Tree-sitter generator. The
`.gitattributes` file marks those files as generated code on GitHub.

To regenerate and test the parser:

```sh
npm run generate
npm test
```
