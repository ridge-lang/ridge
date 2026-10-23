# wc

A small file counter written in Ridge. From `dogfood/wc`, run:

```sh
ridge run -- input.txt
```

For a file containing `the quick brown fox` followed by a newline:

```text
  1 lines
  4 words
  19 bytes (content only, newlines stripped)
  input.txt
```

Words are nonempty runs separated by ASCII whitespace: spaces, tabs, LF,
CR, vertical tabs and form feeds. Repeated separators do not create words.
Other characters, including non-ASCII spaces, remain part of a word.

Lines follow `Fs.lines`: LF and CRLF end a line, a final line ending is
optional, and blank lines inside the file count. The byte count covers line
content, including UTF-8 bytes, with line endings removed as the label says.
