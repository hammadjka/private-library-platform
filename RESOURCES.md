# Private Library Platform Learning Resources

## Knowledge

- [RFC 9110: HTTP Semantics](https://www.rfc-editor.org/rfc/rfc9110.html)
  The primary HTTP semantics standard. Use Sections 9.3.2, 14, 15.3.7, and 15.5.17 for `HEAD`, range requests, `206`, and `416`.
- [RFC 9112: HTTP/1.1](https://www.rfc-editor.org/rfc/rfc9112.html)
  Defines HTTP/1.1 message syntax and connection handling. Use when replacing or evaluating the current handwritten request parser.
- [The Rust Standard Library](https://doc.rust-lang.org/std/)
  Primary reference for the Rust types used by the prototype, including `TcpStream`, `File`, `Read`, `Seek`, and integer parsing.

## Wisdom (Communities)

- [HTTP Working Group: http-core](https://github.com/httpwg/http-core)
  Standards-maintainer issue tracker. Use when an RFC edge case is genuinely ambiguous rather than for routine implementation questions.
- [Rust Users Forum](https://users.rust-lang.org/)
  Moderated practitioner community. Use for concrete Rust ownership, I/O, and library-design questions after reducing them to a small example.
