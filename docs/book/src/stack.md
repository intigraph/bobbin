---
title: The stack
description: How Bobbin fits with the other stack tools
tags: [reference]
status: published
category: appendix
---

# The stack

Bobbin supplies code search and budgeted context. Each tool below works on its
own; [Tambo](https://github.com/intigraph/tambo) can install them together
and check the resulting setup.

| Tool | Role |
|---|---|
| [Tambo](https://github.com/intigraph/tambo) | Installation and verification |
| [Quipu](https://github.com/scbrown/quipu) | Governed knowledge graph |
| [Camayoc](https://github.com/scbrown/camayoc) | Vocabulary and knowledge admission |
| [Bobbin](https://github.com/scbrown/bobbin) | Repository search and context |
| [Yupana](https://github.com/scbrown/yupana) | Code structure and change impact |
| [Desire Path](https://github.com/scbrown/desire-path) | Failed tool-call discovery |

Bobbin's Git coupling identifies files that changed together. Yupana's code
graph answers structural questions about callers and dependencies. These are
different sources of evidence; a co-change is not proof that one function calls
another.

The optional [Quipu integration](guides/quipu-integration.md) adds structured
knowledge beside code search. Builds with the `knowledge` feature expose the
knowledge MCP tools. Local repository search does not require Quipu.

See [architecture](architecture/overview.md) for the data flow and the
[docs map](docs-map.md) for design and research documents.
