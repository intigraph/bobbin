# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**Coverage before 0.6.0 is partial.** v0.5.0, v0.3.0 and v0.2.0 were tagged and
released but have no section here, and 0.4.0/0.3.1/0.1.0 predate the current
format. This is a recorded decision, not an oversight ([#62]): the three missing
ranges are ~543 commits, and machine-generating them would make the pre-history
70% of a file whose job is telling you what changed recently. For complete
history before 0.6.0, use the tag list and `git log v0.4.0..v0.5.0` and friends.

**Section order is maintained by release-please**, which prepends each generated
release directly beneath this header. A hand-added `## [Unreleased]` heading
therefore does not stay at the top — it gets buried under the next release and
reads as if 0.7.2 shipped after 0.8.0, which is how it went wrong twice. Do not
re-add one; unreleased work is described by its conventional-commit subjects
until release-please cuts the section.

[#62]: https://github.com/scbrown/bobbin/issues/62

## [0.32.0](https://github.com/intigraph/bobbin/compare/v0.31.0...v0.32.0) (2026-10-29)


### Features

* **hook:** memory anchor at compaction through the existing compact handler (aegis-gshu.5) ([#218](https://github.com/intigraph/bobbin/issues/218)) ([bad82eb](https://github.com/intigraph/bobbin/commit/bad82eb8e6ef59e70d3f6e3669f79703a5813afc))
* **hook:** per-prompt memory recall lane + item citations in feedback ([#216](https://github.com/intigraph/bobbin/issues/216)) ([eb62da8](https://github.com/intigraph/bobbin/commit/eb62da85a9aecfa4739c9ed13e804b754f7d71cc))
* **prime:** align bobbin prime to the stack prime contract (aegis-km3g.2) ([#215](https://github.com/intigraph/bobbin/issues/215)) ([dd84821](https://github.com/intigraph/bobbin/commit/dd84821f3368d4191d2990f7df532ec3f18e4ce1))
* **recall:** show the cy offline backlog count in the memory recall header (aegis-gshu.7) ([#217](https://github.com/intigraph/bobbin/issues/217)) ([fc7e550](https://github.com/intigraph/bobbin/commit/fc7e550b557070f09a77971acbf81801bff7eef8))


### Bug Fixes

* **release:** report pendant publisher admission before readback ([#209](https://github.com/intigraph/bobbin/issues/209)) ([89b577e](https://github.com/intigraph/bobbin/commit/89b577e19ab099ae6378c913b12066c13712bd9a))

## [0.31.0](https://github.com/intigraph/bobbin/compare/v0.30.0...v0.31.0) (2026-10-24)


### Features

* add verified installer and concise stack guide ([#201](https://github.com/intigraph/bobbin/issues/201)) ([1827292](https://github.com/intigraph/bobbin/commit/182729224e2343d3a53350d437ce1085f4df725c))


### Bug Fixes

* **eval:** refuse contaminated independent L0 measurements ([#210](https://github.com/intigraph/bobbin/issues/210)) ([e010faa](https://github.com/intigraph/bobbin/commit/e010faaf5dc193aaff948b14b2726b2f85e7756e))
* **runtime:** reject noisy ONNX sessions and quiet default diagnostics ([#213](https://github.com/intigraph/bobbin/issues/213)) ([145a272](https://github.com/intigraph/bobbin/commit/145a272eede56839aaac7df44668cb14cb466960))

## [0.30.0](https://github.com/intigraph/bobbin/compare/v0.29.0...v0.30.0) (2026-10-23)


### Features

* publish repository-owned pointer pendants ([#200](https://github.com/intigraph/bobbin/issues/200)) ([66d2d62](https://github.com/intigraph/bobbin/commit/66d2d6251cc9f577444f5013e0eba9588c9a2e38))


### Bug Fixes

* **eval:** bound L0 indexing and isolate sealed index reuse ([#202](https://github.com/intigraph/bobbin/issues/202)) ([9624689](https://github.com/intigraph/bobbin/commit/9624689e5eca15f198910201fd89ebf3afd1af9e))
* **eval:** qualify sealed L0 scoring paths per arm workspace ([#204](https://github.com/intigraph/bobbin/issues/204)) ([3d9073b](https://github.com/intigraph/bobbin/commit/3d9073b41ca6dffc15583852a74911f20396d29a))
* **mcp:** resolve repository-relative chunk reads ([#208](https://github.com/intigraph/bobbin/issues/208)) ([e3296e0](https://github.com/intigraph/bobbin/commit/e3296e04dd7644b8b55f2b1d4c8aea6900a6009a))


### Performance Improvements

* **index:** batch repository file edge clears ([#205](https://github.com/intigraph/bobbin/issues/205)) ([e15b2c3](https://github.com/intigraph/bobbin/commit/e15b2c33ed67a83521350d81298ad4e24b924c8e))

## [0.29.0](https://github.com/scbrown/bobbin/compare/v0.28.0...v0.29.0) (2026-10-16)


### Features

* **hooks:** trigger saved breadcrumb context from keywords ([#194](https://github.com/scbrown/bobbin/issues/194)) ([f5591d3](https://github.com/scbrown/bobbin/commit/f5591d37347ce39fa2399561e0196b65a1a09e33))
* **index:** --maintenance auto, so frequent incremental runs skip a recent sweep ([#196](https://github.com/scbrown/bobbin/issues/196)) ([60529f0](https://github.com/scbrown/bobbin/commit/60529f0fc380d1ae98519b3758f9a1f5848fc4b2))

## [0.28.0](https://github.com/scbrown/bobbin/compare/v0.27.3...v0.28.0) (2026-10-14)


### Features

* **hooks:** discover saved breadcrumbs at session start ([#187](https://github.com/scbrown/bobbin/issues/187)) ([d7d6744](https://github.com/scbrown/bobbin/commit/d7d674486011a7240ddb535a3e1b7a168947daf6))


### Bug Fixes

* **beads:** expose metadata provenance and stale corpus warnings ([#185](https://github.com/scbrown/bobbin/issues/185)) ([c697b91](https://github.com/scbrown/bobbin/commit/c697b913821cb3b5f9aa1dc30d6a3fcdb2fed9e1))
* **deploy:** consume an exact preverified release artifact ([#184](https://github.com/scbrown/bobbin/issues/184)) ([94bfc93](https://github.com/scbrown/bobbin/commit/94bfc9353d9d5f7a256e673c489aa691c3155b6a))

## [0.27.3](https://github.com/scbrown/bobbin/compare/v0.27.2...v0.27.3) (2026-10-09)


### Bug Fixes

* **index:** decode legacy Latin-1 source text ([#182](https://github.com/scbrown/bobbin/issues/182)) ([158aa55](https://github.com/scbrown/bobbin/commit/158aa55365ecd425b9cdc198a5eea18dcceadfb7))

## [0.27.2](https://github.com/scbrown/bobbin/compare/v0.27.1...v0.27.2) (2026-10-08)


### Bug Fixes

* resolve canonical Quipu credentials and suppress refused session writes ([#180](https://github.com/scbrown/bobbin/issues/180)) ([6c93743](https://github.com/scbrown/bobbin/commit/6c93743755322aca5e4920647bbf7d05b66c0534))

## [0.27.1](https://github.com/scbrown/bobbin/compare/v0.27.0...v0.27.1) (2026-10-08)


### Bug Fixes

* **mcp:** keep unrestricted import schemas compatible with object-only clients ([26a6f94](https://github.com/scbrown/bobbin/commit/26a6f945028397685d441f68228bad975f45624e))

## [0.27.0](https://github.com/scbrown/bobbin/compare/v0.26.0...v0.27.0) (2026-10-08)


### Features

* **hooks:** bobbin hooks bundle|install|uninstall|status for Claude AND Codex ([#175](https://github.com/scbrown/bobbin/issues/175)) ([d7b24b8](https://github.com/scbrown/bobbin/commit/d7b24b84feed0f6bf582d809d17d4ca8864ef364))

## [0.26.0](https://github.com/scbrown/bobbin/compare/v0.25.2...v0.26.0) (2026-10-07)


### Features

* **eval:** J4 harness: per-chunk Jev admission + offline A1 report ([#120](https://github.com/scbrown/bobbin/issues/120)) ([a30fea6](https://github.com/scbrown/bobbin/commit/a30fea6623b095713d4665dac88d5f818417527b))

## [0.25.2](https://github.com/scbrown/bobbin/compare/v0.25.1...v0.25.2) (2026-10-01)


### Bug Fixes

* **http:** log query and prompt text at DEBUG; INFO keeps a fingerprint (aegis-uy7boa) ([#170](https://github.com/scbrown/bobbin/issues/170)) ([b21af06](https://github.com/scbrown/bobbin/commit/b21af0682211424f84f90a8fdfac5d710794d0d0))

## [0.25.1](https://github.com/scbrown/bobbin/compare/v0.25.0...v0.25.1) (2026-10-01)


### Bug Fixes

* **deploy:** stop leaking the release work dir on every pull deploy ([#167](https://github.com/scbrown/bobbin/issues/167)) ([0d6ada5](https://github.com/scbrown/bobbin/commit/0d6ada5817a9045773527cc2dbc8a4749394afb8))
* **storage:** fold unindexed rows into the FTS index during maintenance, plus coverage gauges (aegis-4h7zw8) ([#164](https://github.com/scbrown/bobbin/issues/164)) ([4dd2751](https://github.com/scbrown/bobbin/commit/4dd2751ad97baa7c305ea21cecc0bcc5b580ff0b))

## [0.25.0](https://github.com/scbrown/bobbin/compare/v0.24.0...v0.25.0) (2026-09-30)


### Features

* **index:** --include &lt;glob&gt; replaces the config include for one run (aegis-1v555n.1) ([#162](https://github.com/scbrown/bobbin/issues/162)) ([63b418a](https://github.com/scbrown/bobbin/commit/63b418a5f135c3a75127151b45b8120a2554bf0c))


### Bug Fixes

* **knowledge:** escape line jumps and control chars in Turtle literals (aegis-86f2v7.1) ([#165](https://github.com/scbrown/bobbin/issues/165)) ([b482ad1](https://github.com/scbrown/bobbin/commit/b482ad1b6ee957427789c2d3d1ec026744ae5e38))

## [0.24.0](https://github.com/scbrown/bobbin/compare/v0.23.1...v0.24.0) (2026-09-30)


### Features

* **index:** --no-quipu-publish keeps one run's chunks out of Quipu (aegis-1v555n.1) ([#160](https://github.com/scbrown/bobbin/issues/160)) ([7108a6b](https://github.com/scbrown/bobbin/commit/7108a6b6676987d7a63137f003cf79527cfa283b))

## [0.23.1](https://github.com/scbrown/bobbin/compare/v0.23.0...v0.23.1) (2026-09-30)


### Bug Fixes

* **knowledge:** named-graph chunk stage uses quipu's upload_id (aegis-86f2v7) ([#158](https://github.com/scbrown/bobbin/issues/158)) ([4214224](https://github.com/scbrown/bobbin/commit/4214224b7bfc0f5cc6a69181a95a662add39a71f))

## [0.23.0](https://github.com/scbrown/bobbin/compare/v0.22.0...v0.23.0) (2026-09-30)


### Features

* **eval:** a reviewed second resume re-runs one infra-failed task once (aegis-bgk9ho) ([#156](https://github.com/scbrown/bobbin/issues/156)) ([625d0eb](https://github.com/scbrown/bobbin/commit/625d0ebb29d5003b5900f5b49fc0b83e61b050db))
* **eval:** chained pilot continuation that skips both prior cohorts (aegis-bgk9ho) ([#154](https://github.com/scbrown/bobbin/issues/154)) ([d0d2358](https://github.com/scbrown/bobbin/commit/d0d23585bdc2b7e0691a655b62497e76a0d3ef76))
* **eval:** standalone free controls over an explicit task list ([#152](https://github.com/scbrown/bobbin/issues/152)) ([6dd6116](https://github.com/scbrown/bobbin/commit/6dd611691cc83402a60f0418fc4c0f4f73a6d43f))


### Bug Fixes

* **eval:** init gets the task timeout, and a stopped chained run can resume once (aegis-bgk9ho) ([#155](https://github.com/scbrown/bobbin/issues/155)) ([14e3222](https://github.com/scbrown/bobbin/commit/14e32228c264f835f0c543942cd17ce7c8865e4c))
* **eval:** pytest summaries with deselected counts parse; polars-005 installs dateutil (aegis-bgk9ho) ([#153](https://github.com/scbrown/bobbin/issues/153)) ([59f0d42](https://github.com/scbrown/bobbin/commit/59f0d42f11312ad06d847473552eee46ba7d0240))
* **eval:** repair pilot fixtures so the free controls can discriminate ([#150](https://github.com/scbrown/bobbin/issues/150)) ([e2ddc0a](https://github.com/scbrown/bobbin/commit/e2ddc0ad6b8f70a4f3278dc6928a5ac44a7b9d4c))
* **index:** an unreadable file no longer aborts an incremental run (aegis-1v555n.1) ([#157](https://github.com/scbrown/bobbin/issues/157)) ([2e1f8e7](https://github.com/scbrown/bobbin/commit/2e1f8e7f825a688dc01f1bec2a0c67f9d50f5c45))

## [0.22.0](https://github.com/scbrown/bobbin/compare/v0.21.0...v0.22.0) (2026-09-29)


### Features

* **knowledge:** publish the chunk graph into a named quipu graph ([#148](https://github.com/scbrown/bobbin/issues/148)) ([d18dec9](https://github.com/scbrown/bobbin/commit/d18dec910a330cf9da3027384cbb38eeabe7cd2a))

## [0.21.0](https://github.com/scbrown/bobbin/compare/v0.20.1...v0.21.0) (2026-09-27)


### Features

* **eval:** audit interrupted pilots and preflight untouched fixtures ([62b2a76](https://github.com/scbrown/bobbin/commit/62b2a76d38a3c42a3fbfef9f8ff17c6830d87f53))
* **eval:** guard a single nonrepeating pilot continuation ([d294b15](https://github.com/scbrown/bobbin/commit/d294b157e4dba4718a9bfafe80c1ce1187b2c5d3))
* **eval:** guard nonrepeating pilot continuation ([3942a1f](https://github.com/scbrown/bobbin/commit/3942a1fbae2e37069326026f30f33ca47f28677d))
* force graph publication without re-embedding files ([cee44c9](https://github.com/scbrown/bobbin/commit/cee44c91e2d4c9fab8cfd49fed6a079d3c2aada5))


### Bug Fixes

* preserve unchanged files in graph publication snapshots ([4b8ee2a](https://github.com/scbrown/bobbin/commit/4b8ee2a07ba60194bfac0c3261edab47bc0717ba))
* preserve unchanged files in graph publication snapshots ([8e647ad](https://github.com/scbrown/bobbin/commit/8e647ad18a2376e35af588b83bcb1702db3ba33e))
* skip snapshots already published successfully ([d0a4251](https://github.com/scbrown/bobbin/commit/d0a42513f0b2d2151e731a29bd250939fed1cd48))

## [0.20.1](https://github.com/scbrown/bobbin/compare/v0.20.0...v0.20.1) (2026-09-26)


### Bug Fixes

* **deploy:** match release checksum filenames exactly ([7dec602](https://github.com/scbrown/bobbin/commit/7dec602081110fb61cbc40c9a3e58d0b9168ba0a))
* **deploy:** match release checksum filenames exactly ([9a027bb](https://github.com/scbrown/bobbin/commit/9a027bb70d5a40f58cbdfdd33343a2db8b1d059a))

## [0.20.0](https://github.com/scbrown/bobbin/compare/v0.19.1...v0.20.0) (2026-09-26)


### Features

* automatically provision verified GPU runtime for local indexing ([7e5d91b](https://github.com/scbrown/bobbin/commit/7e5d91b87e5edfa2c80bbffe9f9b0e7d48db9b59))
* capture assembly inputs and replay frozen hook responses ([c0edcc4](https://github.com/scbrown/bobbin/commit/c0edcc402a03373947d63d02964cb64f9a99c904))
* capture local context assembly inputs for offline inspection ([400ba39](https://github.com/scbrown/bobbin/commit/400ba3924f4f306ddaf8d87d66439b725e2dc86e))
* **eval:** freeze a reproducible SWE-bench Verified L2 subset ([574c333](https://github.com/scbrown/bobbin/commit/574c333e1ea0da7108596a55ebdd3f5e597ccc79))
* **eval:** prepare a frozen SWE-bench Verified L2 subset ([59c0b60](https://github.com/scbrown/bobbin/commit/59c0b609610875f9971639857e2ff096d51d7ce0))
* **eval:** replay frozen responses through the actual hook ([bbc84a4](https://github.com/scbrown/bobbin/commit/bbc84a4430054a6cf0b3d8b3177291e56b8817c2))
* provision a verified GPU runtime for local indexing ([f0b513d](https://github.com/scbrown/bobbin/commit/f0b513dcec10c5c89af7aaf29215ee0370025f2e))
* **release:** publish artifact provenance and SBOM bundles ([54df90d](https://github.com/scbrown/bobbin/commit/54df90da77785a8e03890b88a61bf91b20c5f3d2))
* **release:** publish provenance and SBOM bundles ([320f26e](https://github.com/scbrown/bobbin/commit/320f26e7f6938e076e08d24a0a3b3b1508d88a39))


### Bug Fixes

* **deps:** bump yanked chacha20 0.10.1 -&gt; 0.10.2 ([76f3929](https://github.com/scbrown/bobbin/commit/76f3929155631c550da24f8f7d77464b7db188cb))
* **deps:** chacha20 0.10.1 (yanked) -&gt; 0.10.2 (aegis-vw9iek) ([868a781](https://github.com/scbrown/bobbin/commit/868a78144842954d4229d2db8022b67f73685241))
* **eval:** reject injection records that disagree with empty output ([7ccec6a](https://github.com/scbrown/bobbin/commit/7ccec6a3ebd1adad1b57dd039edd93ddbd054423))
* **index:** bound retained tokenizer overflow for long records ([538a584](https://github.com/scbrown/bobbin/commit/538a584e1a39b9d5b360f8ec57bd290faeef774a))
* **index:** release unused token overflow between records ([8ae5664](https://github.com/scbrown/bobbin/commit/8ae5664ed7613a4f7596123e3dd28eaeb8ae836d))
* retain exact CLI number encoding in captured baseline ([251410e](https://github.com/scbrown/bobbin/commit/251410e98b07b26f9034a50cf11b7459d9e50d6f))

## [0.19.1](https://github.com/scbrown/bobbin/compare/v0.19.0...v0.19.1) (2026-09-25)


### Bug Fixes

* **eval:** verify pinned ORT and CUDA libraries before L1 work ([565a531](https://github.com/scbrown/bobbin/commit/565a53133433b199b9b2f9a873cc30126ce10ddb))
* **eval:** verify pinned runtime libraries before L1 work ([91df480](https://github.com/scbrown/bobbin/commit/91df480912cb8ded567c225ece4a9c4bfcea51e4))
* let beads manage its own ignore rules ([e77c31b](https://github.com/scbrown/bobbin/commit/e77c31bb27df729fcddbaf21638e46b0bebcab26))
* let beads manage its own ignore rules ([8fccfb5](https://github.com/scbrown/bobbin/commit/8fccfb50b440ab37143f0b05e165d5322590d347))
* **storage:** detect an existing FTS index instead of training one per request ([2bc9ca6](https://github.com/scbrown/bobbin/commit/2bc9ca667ef7101c8e1719f4873fc667fa8e27d2))
* **storage:** detect an existing FTS index instead of training one per request (aegis-mgpp28) ([19bcdee](https://github.com/scbrown/bobbin/commit/19bcdee0cb046d7a9936818d653f35decb5e8c71))

## [0.19.0](https://github.com/scbrown/bobbin/compare/v0.18.1...v0.19.0) (2026-09-24)


### Features

* **eval:** add explicit offline Jev rerank and prompt-gate replay ([b1bf482](https://github.com/scbrown/bobbin/commit/b1bf482414860f0d40e97c39b11fb25d0548ce28))
* **eval:** add semantic knapsack replay and recorded-response CLI ([681e739](https://github.com/scbrown/bobbin/commit/681e739916f02c1fa4c6ee0daf9a90036a649413))
* **eval:** offline Jev rerank, semantic packing and prompt-gate replay ([0ff34c1](https://github.com/scbrown/bobbin/commit/0ff34c1c37a546629effcb68b220c2ce5406597c))


### Bug Fixes

* **eval:** distinguish turn exhaustion from unavailable agents ([40b850c](https://github.com/scbrown/bobbin/commit/40b850c9b2100d9cf36b9bcddbbf83861634f590))

## [0.18.1](https://github.com/scbrown/bobbin/compare/v0.18.0...v0.18.1) (2026-09-24)


### Bug Fixes

* **eval:** enforce preregistered pilot cells and grading controls ([#118](https://github.com/scbrown/bobbin/issues/118)) ([cd8a270](https://github.com/scbrown/bobbin/commit/cd8a270221faf6664d0380d6a27d43fe31863f73))
* **eval:** l0 uses the workspace clone_repo returns, and imports setup_bobbin ([#115](https://github.com/scbrown/bobbin/issues/115)) ([13ebca0](https://github.com/scbrown/bobbin/commit/13ebca0256c68075c67e6e010fe2dee8a932ae5e))

## [0.18.0](https://github.com/scbrown/bobbin/compare/v0.17.1...v0.18.0) (2026-09-24)


### Features

* **eval:** v2 L0 offline retrieval harness ([#109](https://github.com/scbrown/bobbin/issues/109)) ([2b752f8](https://github.com/scbrown/bobbin/commit/2b752f8007c92425d08981ccdfe0371c13b68669))
* **http:** /status lists the indexed repos ([#113](https://github.com/scbrown/bobbin/issues/113)) ([f013017](https://github.com/scbrown/bobbin/commit/f013017121f9f96aa2bb7f8954557a2f928881b3))


### Bug Fixes

* **ort:** resolve symlinks before locating the bundled ONNX Runtime ([#107](https://github.com/scbrown/bobbin/issues/107)) ([d14adae](https://github.com/scbrown/bobbin/commit/d14adaee56eeb1525199b25eca28a21977b29ff4))

## [0.17.1](https://github.com/scbrown/bobbin/compare/v0.17.0...v0.17.1) (2026-09-20)


### Bug Fixes

* **ci:** the size gate could not see an untracked file (aegis-wbbycq) ([#106](https://github.com/scbrown/bobbin/issues/106)) ([2410b9a](https://github.com/scbrown/bobbin/commit/2410b9aeb7de2b864629c1aabb206405c7fef056))
* **mcp:** serve the configured remote, not an empty local index (aegis-wbbycq) ([#104](https://github.com/scbrown/bobbin/issues/104)) ([23e09d1](https://github.com/scbrown/bobbin/commit/23e09d192aa38bdc11155e48721a8dc1260b24b5))

## [0.17.0](https://github.com/scbrown/bobbin/compare/v0.16.4...v0.17.0) (2026-09-16)


### Features

* **beads:** read beads from a br JSONL export, not only Dolt ([493bbdf](https://github.com/scbrown/bobbin/commit/493bbdf44865736bba5c319dc77823bd56bd8f77))
* **beads:** read beads from a br JSONL export, not only Dolt ([db67da3](https://github.com/scbrown/bobbin/commit/db67da36383389c845276f726d6f12db9d6b55ba))


### Bug Fixes

* **ci:** keep index.rs at its ceiling; raise config.rs's deliberately ([bf33846](https://github.com/scbrown/bobbin/commit/bf3384612f4c43d95e5e0d05c983e9fa771a8c6e))

## [0.16.4](https://github.com/scbrown/bobbin/compare/v0.16.3...v0.16.4) (2026-09-16)


### Bug Fixes

* **eval:** a run with ZERO tests executed is not a pass ([#99](https://github.com/scbrown/bobbin/issues/99)) ([16300d3](https://github.com/scbrown/bobbin/commit/16300d3258f1c407ab32b016993fd76668394cc6))
* **eval:** scope settings isolation to the subprocess, not the whole host ([#98](https://github.com/scbrown/bobbin/issues/98)) ([5fd5d41](https://github.com/scbrown/bobbin/commit/5fd5d416afe3892fc67b604cbf2df60781c4d78c))

## [0.16.3](https://github.com/scbrown/bobbin/compare/v0.16.2...v0.16.3) (2026-09-13)


### Bug Fixes

* **release:** scrub complete tracker references from changelogs ([3a74009](https://github.com/scbrown/bobbin/commit/3a74009b1605ab2e2c6c537aa0b6af7cf51b21d1))

## [0.16.2](https://github.com/scbrown/bobbin/compare/v0.16.1...v0.16.2) (2026-09-07)


### Bug Fixes

* **index:** preserve distinct same-line chunk identities ([#91](https://github.com/scbrown/bobbin/issues/91)) ([d98cba5](https://github.com/scbrown/bobbin/commit/d98cba545d196d8d19978e456d0b0382984a184d))

## [0.16.1](https://github.com/scbrown/bobbin/compare/v0.16.0...v0.16.1) (2026-09-06)


### Bug Fixes

* **knowledge:** preserve UTF-8 across snapshot upload parts ([b1bc5ff](https://github.com/scbrown/bobbin/commit/b1bc5ff8fb86263954cffca4c45a72867bbc885a))

## [0.16.0](https://github.com/scbrown/bobbin/compare/v0.15.0...v0.16.0) (2026-09-05)


### Features

* **mcp:** annotate all 35 tools, and guard that they stay annotated ([48fa6e4](https://github.com/scbrown/bobbin/commit/48fa6e4e779118caaf952771ddccee61cf2fc757))


### Bug Fixes

* **mcp:** make the knowledge helpers reachable from the split tool modules ([ff4f405](https://github.com/scbrown/bobbin/commit/ff4f405aa5b197e62b3c0df0545151a7afa477a4))

## [0.15.0](https://github.com/scbrown/bobbin/compare/v0.14.0...v0.15.0) (2026-09-05)


### Features

* **hook:** read tool_response, so the injection can answer what you FOUND (aegis-dke1fo) ([ec99661](https://github.com/scbrown/bobbin/commit/ec996612a58d939bb84288bedbfce550ae76c825))


### Bug Fixes

* **archive:** one shared recent-record listing, so MCP and HTTP cannot diverge ([d3ebbfe](https://github.com/scbrown/bobbin/commit/d3ebbfe72a7eb8d000db09ad18d33918b90dd272))
* **ci:** assert the full-build gate as a property, not a spelling (aegis-g2gpw5) ([909c61e](https://github.com/scbrown/bobbin/commit/909c61ebd80869a1f8af51acc250ad02e8eac523))
* **release:** build+publish on releases_created in the same run (aegis-g2gpw5) ([a24cddb](https://github.com/scbrown/bobbin/commit/a24cddb093aa60ffac4a313a08139380d0e21111))

## [0.14.0](https://github.com/scbrown/bobbin/compare/v0.13.1...v0.14.0) (2026-09-04)


### Features

* **knowledge:** proxy canonical Quipu shares [aegis-hd9yu] ([7c70a9b](https://github.com/scbrown/bobbin/commit/7c70a9bb58812b27653b9979f60e2ddf36972efb))


### Bug Fixes

* **release:** address repository when publishing draft ([820d749](https://github.com/scbrown/bobbin/commit/820d74968fe6310ab4ff70f6f28a9c8037586025))

## [0.13.1](https://github.com/scbrown/bobbin/compare/v0.13.0...v0.13.1) (2026-09-03)


### Bug Fixes

* **release:** publish only after assets upload [aegis-yozpp0] ([8a1a1c5](https://github.com/scbrown/bobbin/commit/8a1a1c5faf823e25871fcdadcf3d0e3dd6eee05f))

## [0.13.0](https://github.com/scbrown/bobbin/compare/v0.12.4...v0.13.0) (2026-09-03)


### Features

* **pack:** add verified portable index artifacts ([55d5a41](https://github.com/scbrown/bobbin/commit/55d5a4168460851e43bfc783bfbd28b7f1557a04))
* **release:** publish repository index pack [aegis-otg3xz.2] ([9fc05f9](https://github.com/scbrown/bobbin/commit/9fc05f94ba0b0951153d1c444faca83e1cdb26ed))


### Bug Fixes

* **ci:** split pack reindex helper from index CLI [aegis-fm02xf] ([f41bb77](https://github.com/scbrown/bobbin/commit/f41bb77bb810e0b1241832ffcb218b5a0f9e45a0))

## [0.12.4](https://github.com/scbrown/bobbin/compare/v0.12.3...v0.12.4) (2026-09-02)


### Bug Fixes

* **calibrate:** satisfy source size ratchet ([ac6e294](https://github.com/scbrown/bobbin/commit/ac6e294a7be4e3e8daf7ba7ec5dffcc89c2ccc68))

## [0.12.3](https://github.com/scbrown/bobbin/compare/v0.12.2...v0.12.3) (2026-09-02)


### Bug Fixes

* **calibrate:** cap default sweep at 300 probes ([b11e700](https://github.com/scbrown/bobbin/commit/b11e70071e3025126d8f0d6afaf3c8231de56775))

## [0.12.2](https://github.com/scbrown/bobbin/compare/v0.12.1...v0.12.2) (2026-09-02)


### Bug Fixes

* **knowledge:** poll atomic snapshot promotion ([ad6fbc3](https://github.com/scbrown/bobbin/commit/ad6fbc3687ef38e089ba1bc71627de830b170d8b))

## [0.12.1](https://github.com/scbrown/bobbin/compare/v0.12.0...v0.12.1) (2026-09-02)


### Bug Fixes

* **knowledge:** allow bounded snapshot promotion ([62fe671](https://github.com/scbrown/bobbin/commit/62fe671ca9212bef4d4e5878a54e5a7f4e38f8e5))

## [0.12.0](https://github.com/scbrown/bobbin/compare/v0.11.1...v0.12.0) (2026-09-02)


### Features

* **knowledge:** publish chunk snapshots resumably ([5ba6d91](https://github.com/scbrown/bobbin/commit/5ba6d918a480899d098c6aeae8be2c2647f58dab))

## [0.11.1](https://github.com/scbrown/bobbin/compare/v0.11.0...v0.11.1) (2026-09-01)


### Bug Fixes

* **index:** allow production snapshot pushes to finish ([783c076](https://github.com/scbrown/bobbin/commit/783c0760d5b24bfedd03159e2e85d1fb08cf7131))

## [0.11.0](https://github.com/scbrown/bobbin/compare/v0.10.4...v0.11.0) (2026-08-31)


### Features

* **index:** opt-in HTML documents source through the file lifecycle (bobbin-e0d) ([a6482d7](https://github.com/scbrown/bobbin/commit/a6482d788bdc652f2f5208ef4b154814f8ba921a))
* **search:** opt-in cross-encoder reranking stage for hybrid search (bobbin-cf8) ([660b4fb](https://github.com/scbrown/bobbin/commit/660b4fbe84969405c022428a8cf5ef10a109bf17))
* **similar:** persist similar_to chunk edges from the near-duplicate scan (bobbin-363) ([2046e21](https://github.com/scbrown/bobbin/commit/2046e21953a5d06c47a630549facd52c07148b69))


### Bug Fixes

* **ci:** ignore release-please CHANGELOG in markdownlint ([f223a9c](https://github.com/scbrown/bobbin/commit/f223a9c7798e392e1c161a48dffa42ea77db5b20))
* **search:** fail loud when live bead enrichment fails ([e5fd98f](https://github.com/scbrown/bobbin/commit/e5fd98f835985a75b488fa01d799ea430b29fbe8))

## [0.10.4](https://github.com/scbrown/bobbin/compare/v0.10.3...v0.10.4) (2026-08-30)


### Bug Fixes

* **index:** fail on dropped chunk graph pushes ([9bd3c7b](https://github.com/scbrown/bobbin/commit/9bd3c7bd09db9998fdbd6486a73c65ccf75ac9bc))

## [0.10.3](https://github.com/scbrown/bobbin/compare/v0.10.2...v0.10.3) (2026-08-29)


### Bug Fixes

* **index:** bound remote graph publication stalls ([ad1b499](https://github.com/scbrown/bobbin/commit/ad1b499d1745dae9c232c3c7bb89b1a8bfe0533e))

## [0.10.2](https://github.com/scbrown/bobbin/compare/v0.10.1...v0.10.2) (2026-08-29)


### Bug Fixes

* **http:** authenticate push webhooks with HMAC (aegis-svpnm) ([2d69e39](https://github.com/scbrown/bobbin/commit/2d69e3924455b4c8c1a0b328024b981d7254fd9a))

## [0.10.1](https://github.com/scbrown/bobbin/compare/v0.10.0...v0.10.1) (2026-08-29)


### Bug Fixes

* **runtime:** load ONNX from Linux multiarch paths ([fba9424](https://github.com/scbrown/bobbin/commit/fba9424517b522ba38992c28ddf65a9871c4f692))

## [0.10.0](https://github.com/scbrown/bobbin/compare/v0.9.1...v0.10.0) (2026-08-29)


### Features

* **knowledge:** add Quipu share v1 contract fixtures ([c8d11c0](https://github.com/scbrown/bobbin/commit/c8d11c0295d40e472263c90ce8e9823a20f699cf))

## [0.9.1](https://github.com/scbrown/bobbin/compare/v0.9.0...v0.9.1) (2026-08-27)


### Bug Fixes

* **ci:** keep Lance conflict test within size ratchet ([5eccd8e](https://github.com/scbrown/bobbin/commit/5eccd8eebd85672ee9692e5f479bfc5d528bfa87))
* **ci:** refresh hook file-size ratchets ([f7c0954](https://github.com/scbrown/bobbin/commit/f7c09545e1c140d4b11f49dce59f5e144a88bf0d))
* **hook:** align local work-item context scope ([a5d533f](https://github.com/scbrown/bobbin/commit/a5d533fb6a02db914c14eb6db031ac619401f837))
* **lance:** recognize lowercase retryable commit conflicts ([0637ec7](https://github.com/scbrown/bobbin/commit/0637ec744d466abda5800dd36a84ad56c4196fbc))
* **package:** publish as bobbin-ai with registry Quipu ([25cfafc](https://github.com/scbrown/bobbin/commit/25cfafc608979a600547583fff072af8c6c71def))

## [0.9.0](https://github.com/scbrown/bobbin/compare/v0.8.0...v0.9.0) (2026-08-25)


### Features

* **cli:** wire the advanced query parser into `bobbin search` ([905c291](https://github.com/scbrown/bobbin/commit/905c2916512a066189a4056b516e1230b4672054))
* **http:** add GET /deps and GET /history for MCP/HTTP parity ([63b2309](https://github.com/scbrown/bobbin/commit/63b2309571780b8fe6bc0a6e51aa2d3d904e4787))
* **index:** add `bobbin index-bead <id>` single-bead incremental reindex ([0622f35](https://github.com/scbrown/bobbin/commit/0622f35926962ca47d24f7fa21be2f7b4b4dc175))
* **reactions:** wire search-based reactions into the hook, with a no-results signal and real latency ([3a6844f](https://github.com/scbrown/bobbin/commit/3a6844fc8c4b98706b5e957a63e1f262467aaa10))
* **search:** echo the parsed query on /search and make `+` a real required-term operator ([53a32b3](https://github.com/scbrown/bobbin/commit/53a32b338cdcc50a34e13123edbfdab5657edaa1))
* **tripwire:** surface governed path boundaries in injected context ([c45b576](https://github.com/scbrown/bobbin/commit/c45b576e2d7e3acc52ee7459fbdfcf0b6a1a97ac))


### Bug Fixes

* **docs:** accept established technical vocabulary ([9061580](https://github.com/scbrown/bobbin/commit/90615807ca6e1c796d774985d0acc1722ea0f4c2))
* **knowledge:** parse the live IRI lane in expand_knowledge, not the superseded one ([108b5e9](https://github.com/scbrown/bobbin/commit/108b5e93426129dfc4406c602534ddf25580846b))
* **mcp:** push bead filter into search ([ce86add](https://github.com/scbrown/bobbin/commit/ce86add711ef55bd14db365e730c277fcd1d7f51))
* **release:** publish deployable Linux asset per target ([28088ce](https://github.com/scbrown/bobbin/commit/28088ce6976b5a953b614f0dfa038e1011b945fc))

## [0.8.0](https://github.com/scbrown/bobbin/compare/v0.7.3...v0.8.0) (2026-08-24)

### Features

- inject faction-scoped NA grounding context ([e543c92](https://github.com/scbrown/bobbin/commit/e543c92da4691279dea10f6ccc9f10a04cb351a7))

## [0.7.3](https://github.com/scbrown/bobbin/compare/v0.7.2...v0.7.3) (2026-08-24)

### Bug Fixes

- **index:** recover FTS compaction panics truthfully ([cc3a2e2](https://github.com/scbrown/bobbin/commit/cc3a2e22c6d44fdc90f89610a132fb9468a0800f))
- **release:** replace package-bound release-plz lane ([fdbe086](https://github.com/scbrown/bobbin/commit/fdbe086178c9ff649b2145c03227cd82eb092c3b))

## [0.7.2] - 2026-08-24

### Added

- publish authenticated, replaceable chunk snapshots to a remote Quipu endpoint
- map indexed modules, symbols, documents, and sections into the governed code-entity vocabulary

### Fixed

- mint code and document IRIs on the live ingest lane so entities merge with the
  existing code graph instead of forking a second one beside it
- encode the IRI characters that make a whole Turtle document unparseable, not
  just their own triple

## [0.7.1] - 2026-08-24

### Fixed

- recover transient FTS failures during cross-process index churn with bounded backoff
- expose FTS rebuild and terminal search-error counters for keyword and hybrid modes
- verify release checksums against final asset names and retire the stale Forgejo deploy path

## [0.7.0] - 2026-08-23

Release the chunk-graph, generic source, deterministic extraction, and Quipu snapshot
work that accumulated after v0.6.7. This is a minor release because it adds public CLI,
MCP, indexing, and knowledge-integration capabilities.

### Added

- chunk adjacency and ownership edges, with edge-aware context expansion
- chunk-neighbor MCP tooling and work-item-keyed context injection
- generic `ChunkSource` ingestion, including SQL, commit, and archive sources
- deterministic entity extraction and chunk-to-entity reconciliation
- diffed chunk-graph snapshot export to Quipu
- quarantined inferred extraction with an explicit confidence envelope

### Fixed

- join the live Aegis code-entity namespace
- preserve complementary non-adjacent expansion results
- record the serving model and refuse model-mixed evaluation aggregation
- install `protoc` before release package verification

## [0.6.7] - 2026-08-07

Bootstrap the repository-owned release baseline. Release-plz can now package
the pinned Quipu dependency and derives Bobbin's version from `v*` tags instead
of the unrelated crate on crates.io.

### Added

- *(deploy)* pull-based deploy from a published release artifact

### Fixed

- *(release)* derive versions from repository tags
- *(release)* make the pinned Quipu dependency packageable

## [0.6.6] - 2026-08-04

Release: the ks9cl P0 finally has a name

### Added

- *(mcp)* knowledge tools now federate the ontology, and say which graph answered([25e38cf](https://github.com/scbrown/bobbin/commit/25e38cf3209910e79811aba5ed92848b91c1ba74))

### Fixed

- *(mcp)* a server restart permanently broke every live agent's MCP tools([4958748](https://github.com/scbrown/bobbin/commit/4958748ca25011b792e5729c11689d75785bdb02))
- *(deploy)* the glibc-safe build image has no protoc, so the build dies mid-run([9e1f716](https://github.com/scbrown/bobbin/commit/9e1f7166b57f608ee284478f09cfe2c228f9cc01))
- *(mcp)* trim knowledge payloads so the answer can actually be received([12b7d07](https://github.com/scbrown/bobbin/commit/12b7d074f302f38e812b1a066a2b5650d680c73f))

### Changed

- *(mcp)* federate via the EXISTING quipu_endpoint key, not a second knob([74ac8c9](https://github.com/scbrown/bobbin/commit/74ac8c9adeeb70a9b5bbe2497fe3aef8a5e6f189))

### Documentation

- *(deploy)* make the glibc-safe container build a RECIPE, not an emergency procedure([7f2ecbd](https://github.com/scbrown/bobbin/commit/7f2ecbd8ed1596ba1339ebc6cde8974196c5ce6c))

## [0.6.5] - 2026-08-02

Release: the first release since v0.6.0 that can actually build

### Added

- *(deploy)* fail the cutover on a featureless binary, not just a glibc mismatch([7aafb2c](https://github.com/scbrown/bobbin/commit/7aafb2c51ce69b417faeff2b9c0fc6d6a6feae85))
- *(version)* emit the build git sha so a deploy is verifiable (/version + --version)([300b9c0](https://github.com/scbrown/bobbin/commit/300b9c06bfc8e2f0282645dc07967a0fd35ed370))

### Fixed

- *(deploy)* build the shipped binary WITH --features knowledge([864c710](https://github.com/scbrown/bobbin/commit/864c710d34123adea091885f52b0f41773a6bd8a))
- *(beads)* /beads pushes the Issue filter into LanceDB instead of over-fetch-then-filter([2eba8f2](https://github.com/scbrown/bobbin/commit/2eba8f29447ec57da16c1eb6522f49eca1e7154a))
- *(lance)* bound compaction memory and prune before compacting([585a02e](https://github.com/scbrown/bobbin/commit/585a02eece732de9d903a6d0aea4091cca0dea4e))
- *(embed)* bound the embed batch at the chokepoint — a whole corpus reached the model in one call([b7f340b](https://github.com/scbrown/bobbin/commit/b7f340b38cd0a1ea92a02bc19e7206b8bcfe61af))
- *(lance)* maintenance must sweep every table, not just chunks([90ea77a](https://github.com/scbrown/bobbin/commit/90ea77ad15000bb4200f55b7b6b62b0789ed0dbf))
- *(lance)* scheduled maintenance WAITS for the lock; a skip is no longer silent([2795d71](https://github.com/scbrown/bobbin/commit/2795d71877658d4e26f8fa6ac565108116d10c3f))
- *(lance)* one lock acquisition per sweep — a per-op wait multiplies by 27 repos([3b68aea](https://github.com/scbrown/bobbin/commit/3b68aeaedd4c63995e90d78a10d5c21da29964a6))
- *(release)* the aarch64 leg OOM-kills the runner under fat LTO — 4 tags, 0 releases([f84f7b7](https://github.com/scbrown/bobbin/commit/f84f7b758fe7d3bb58b0b4c46e1a43a086a95851))

## [0.6.4] - 2026-07-23

Release: deploy the knowledge/PPR build to the serving host (#56)

### Added

- *(config)* feature-gate a non-zero default ppr_weight (0.3) on knowledge builds([297f77c](https://github.com/scbrown/bobbin/commit/297f77c1bdc18bae545a3e809097af6fe38068cf))

### Fixed

- *(release)* match release-plz git_tag_name to the repo's v-prefix tags([e064e56](https://github.com/scbrown/bobbin/commit/e064e5634add7c1ac5d5d53fe379e475b02aaa5c))
- *(release)* move publish restriction out of the manifest so release-plz opens PRs([a8cffbe](https://github.com/scbrown/bobbin/commit/a8cffbe784815cdf7677c4fb14253cf3e249688a))
- *(serve)* wire the Quipu store into the HTTP /context path so PPR runs there too([4c6ed6e](https://github.com/scbrown/bobbin/commit/4c6ed6e1d43626fdcd95f2c23b19dd058119fdba))

### Documentation

- *(plans)* status-label the 3 hardening/metrics/hooks plans vs main([aa3f24c](https://github.com/scbrown/bobbin/commit/aa3f24c2f84e14877b0847587dfc5b44f471619c))
- *(plans)* label 5 plan docs with implementation status + frontmatter for 4 guide pages([ef2714a](https://github.com/scbrown/bobbin/commit/ef2714a3c591f3ea8504ee340adaec2b9a337563))
- *(plans)* status-label the 4 eval plans — all implemented/record (sweep) (#48)([3c324e7](https://github.com/scbrown/bobbin/commit/3c324e7af60b9a641f1c0876673275bbe5361cbd))
- *(plans)* label 4 plan docs with verified implementation status (#49)([a9ca093](https://github.com/scbrown/bobbin/commit/a9ca0933611c9fb880e8330db0074fc2ca79a19f))
- *(plans)* status-label the last 6 plan docs — 3 done, 2 dark-behind-a-feature, 1 backlog([6d14759](https://github.com/scbrown/bobbin/commit/6d147594a136615a72dcc90a1cd6dcb58b38484e))
- *(design)* status-label the 8 design docs outside docs/plans/([786834e](https://github.com/scbrown/bobbin/commit/786834e71b50fec929474317d88eeccb45df384b))
- *(plans)* note the named-graph (quipu #36/#49) substrate for subset-export (#57)([38a1c53](https://github.com/scbrown/bobbin/commit/38a1c53a783f344868025fc43bc4331de8cab300))
- *(design)* correct micro-ui status ⬜→🟡 — the embedding IS built([13ec199](https://github.com/scbrown/bobbin/commit/13ec199ef9c37d18a2fe76320dd18b7416f60411))
- label implementation status of roadmap + architecture (plan sweep)([9e35d57](https://github.com/scbrown/bobbin/commit/9e35d57f9cdddb7a3dca74f927173efe0a945295))

## [0.6.3] - 2026-07-23

Release: the structural-backend seam callers + purge watermark fix, released via the glibc-safe runner build for the serving-host deploy lane

### Added

- *(analysis)* the structural-backend seam — swappable engine contract for refs/symbols/impact([6bb49f8](https://github.com/scbrown/bobbin/commit/6bb49f853779d74927cdb03a9d22e2acd307e2cc))
- *(analysis)* route callers through the structural-backend seam; impact goes behind it([5b7fc3a](https://github.com/scbrown/bobbin/commit/5b7fc3a2ed7f689178eb61633dd617b176c41825))

### Fixed

- *(index)* purge resets the per-repo commit watermark so re-index rebuilds full history([a1d5871](https://github.com/scbrown/bobbin/commit/a1d587129020b459c3b55bdaeeb8e8ad521ede6a))

## [0.6.2] - 2026-07-23

Release: glibc-safe deploy tooling + the calibrate fix, plus the release-plz adoption trail

### Added

- *(deploy)* glibc-safe build + gated, rollback-capable cutover([3b52b9a](https://github.com/scbrown/bobbin/commit/3b52b9aaf342228b0c54f72dc6d89d0daeeb7eb7))

### Fixed

- *(release)* track Cargo.lock — release-plz cannot determine versions without it([bbd8b3f](https://github.com/scbrown/bobbin/commit/bbd8b3f874a07b983d43cde1454578a1cb091207))
- *(release)* declare publish = false — the crates.io 'bobbin' is an unrelated crate([3ccdcc5](https://github.com/scbrown/bobbin/commit/3ccdcc581e7793363f2fba714a1a4ffa949499c5))
- *(calibrate)* auto-calibration failed every run — passed source tree as calibrate path instead of bobbin home([f94f7a0](https://github.com/scbrown/bobbin/commit/f94f7a0e31812275197ebcd7579b73e06c8deef3))
- *(release)* untrack committed-but-gitignored data files — they fail release-plz's clean-tree check([0ea99cd](https://github.com/scbrown/bobbin/commit/0ea99cd7f7ca2613ffa73cf38e0c6e82d68da49a))
- *(release)* release = true — release-plz skips publish=false crates by default([9563aae](https://github.com/scbrown/bobbin/commit/9563aaeac13a932d59afe4abdb453609edd6ed74))

### CI/CD

- *(release)* adopt release-plz — config AND the job that executes it([749984c](https://github.com/scbrown/bobbin/commit/749984c048ecb024b121b2c59864b50869cf808c))

## [0.6.1] - 2026-07-22

Release: the paa8 repo-scoped index state + m2ob compaction lock/retry release

### Added

- *(index)* index yaml/jinja/terraform/shell so IaC repos are searchable([c35d632](https://github.com/scbrown/bobbin/commit/c35d632dcb54c9052ac2a068a81dfb4f8e0f37fb))

### Fixed

- *(knowledge)* build the quipu feature, and make PPR actually rerank([296238c](https://github.com/scbrown/bobbin/commit/296238cdec6f8cc737fd0c90699c8f4dbe8e2ead))
- *(deps)* choose the quipu rev deliberately, and say why([048e1bc](https://github.com/scbrown/bobbin/commit/048e1bce70200abe65c043ac1145eee1cfcb7661))
- *(index)* repo-scope ALL incremental state — hashes, watermarks, deletes — and give every store one consistent key([3eaced2](https://github.com/scbrown/bobbin/commit/3eaced2a68a16a30c819a502ca492108b4e0fb52))
- *(lance)* single-compactor gate + commit-conflict retry — stop the indexer/server table race([4c02576](https://github.com/scbrown/bobbin/commit/4c025769aff38f9c36667d64db09ee8a12b0294d))

### Documentation

- *(plans)* mark PPR plan as dark — knowledge feature enabled by no build path([37d70c5](https://github.com/scbrown/bobbin/commit/37d70c5382f8abf98e61e88f4a806fc7f2229300))

## [0.6.0] - 2026-07-13

Multimodal PDF ingest, index-freshness safety net, and two indexing/telemetry
correctness fixes.

### Added

- **Multimodal ingest — PDF text (bo-j5r0)** — opt-in `[index] multimodal`
  flag. When enabled, `bobbin index` also walks `**/*.pdf`, extracts text via a
  pure-Rust extractor (`pdf-extract`; no Python/native toolchain), and chunks it
  like a plain-text document (`language = "pdf"`) so runbooks, design docs, and
  specs become searchable. Off by default — no change to the default
  dep/behavior profile. Image captioning (vision LLM) is tracked as a follow-up.
- **Periodic reindex backstop for `watch` (#44)** — `bobbin watch` now runs a
  periodic full-tree reconciliation (on by default, every 15 min;
  `--reindex-interval-secs`, `0` disables). Each sweep re-embeds files whose
  content hash drifted and prunes rows for files that vanished from disk,
  catching events the file watcher dropped. Sweeps are incremental, so one where
  the watcher kept up does almost no work.
- **Index freshness signal in `status` (#44)** — `bobbin status` reports a
  `Freshness` line (and JSON field) that flags the index stale when the current
  git HEAD commit is newer than the last index run. Uses commit time, not
  wall-clock, so a quiet repo is never a false positive.

### Fixed

- **Batched prune delete (#43)** — pruning a source with more than SQLite's
  `SQLITE_MAX_VARIABLE_NUMBER` (32766) files in one pass no longer exceeds the
  bound-variable limit and aborts. The `DELETE … IN (…)` is chunked within a
  single transaction, keeping the prune atomic and the index consistent.
- **Hook injection count in remote deployments (#42)** — `bobbin hook status`
  reported `Injection count: 0` while injection was firing. The remote inject
  path now advances `hook_state`, and `hook status` resolves the bobbin root the
  same way the inject path does (first ancestor with `.bobbin/config.toml`), so
  the reported count is accurate and no longer depends on CWD.

## [0.4.0] - 2026-06-27

Search quality, knowledge-graph ranking, workflow telemetry, and a bead
access-control hardening.

### Added

- **Personalized PageRank ranking signal** — `search::ppr` folds a bounded
  graph-connectivity boost (seeded by the top hybrid hits, computed via
  `quipu::page_rank` over the `co_changed_with` coupling graph) into context
  ranking. Off by default; enable with `--ppr-weight` / `[search] ppr_weight`.
  Eval harness gains `calibrate --ppr-weights` for tuning.
- **Workflow telemetry (GH#9)** — `bead_lineage` store + `bobbin bead
  link`/`history` (Layer 1); automatic bead→commit association from `Bead*`
  commit trailers during indexing (Layer 1.5); `bobbin bundle additions` and
  `bundle drift` over the lineage (Layer 2).
- **Ontology (GH#14)** — ontology-aware search (tag/bundle hierarchy
  expansion), `GET /ontology` + `/ontology/{tag}` REST endpoints, and
  `bobbin ontology infer` (candidate concepts from coupling communities).
- **Beads indexing (GH#13)** — index bead labels and `metadata`; incremental
  bead indexing (content-hash skip); `[beads] exclude_labels` keeps sensitive
  beads (e.g. `security`, `escalation`) out of the index entirely.

### Fixed

- **FTS 500 (GH#21)** — keyword/`--type` search no longer 500s with "Failed to
  collect FTS results"; the index self-heals (rebuild + retry) and is rebuilt
  after `watch` compaction.
- **Hook status (GH#10)** — detects project-level hooks whose commands are
  wrapped with env prefixes / absolute paths / `|| true`.
- `--type` help and MCP schemas now list all valid chunk types (incl.
  `issue`/`bead`, `commit`, `doc`).

### Security

- **Bead access control** — bead chunks (`beads:<rig>:<id>`) are now
  access-scoped to their rig, so per-rig allow/deny rules apply to beads exactly
  as to code (previously they bypassed deny rules and could expose all beads).

## [0.3.1] - 2026-03-24

### Added

- `bobbin connect <url>` command — server-first setup with auto hook install
- Auto-detect forge type (GitHub, GitLab, Forgejo, Bitbucket) for source URL deep links
- Deep linking support in web UI (`#search?q=foo`, `#context?q=bar`)
- Repo/tag/group filter controls on the web UI search page
- Feedback CLI command with server mode proxy (GH#7)
- `--repo-root` flag for cross-repo deep bundle view
- Multi-agent onboarding improvements — `BOBBIN_SERVER` env hints + hook install (GH#4)
- Inline query syntax reference in search guide (`repo:`, `lang:`, `type:`, `file:`, `tag:`, `group:`)

### Fixed

- Watch service now detects per-file git repo names (GH#2)
- Hook status walks up directory tree to find parent settings.json (GH#3)
- Bundle/tags discovery walks up directory tree past git roots (GH#5)
- Normalize absolute file paths to repo-relative in `bundle add` (GH#6)
- Dynamic `repo_path_prefix` replaces hardcoded `/var/lib/bobbin/repos/` path

## [0.1.0] - 2026-02-07

### Added

- Code indexing with tree-sitter parsing for Rust, TypeScript, Python, Go, Java, and C++
- Semantic search using ONNX Runtime embeddings (all-MiniLM-L6-v2)
- Full-text keyword search via LanceDB/tantivy
- Hybrid search combining semantic and keyword results with Reciprocal Rank Fusion
- Git history analysis for temporal context
- Coupling detection between files based on co-change patterns
- MCP server for AI agent integration
- CLI with `index`, `search`, `grep`, `mcp-server`, and `completions` subcommands
- LanceDB as primary vector storage with SQLite for coupling metadata
- Support for `.bobbinignore` exclude patterns
