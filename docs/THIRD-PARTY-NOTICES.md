# Third-Party Notices（部分依赖）

本文件仅记录 2026-09-20 在 `deny.toml` 新增许可证例外的 **7 个锁定版本**，不是全部第三方依赖、SBOM 或完整许可证合规声明，也不代表每个平台的制品均包含这些包。实际分发应以该制品的依赖及构建输入为准。

许可证标识来自各版本源码包的 `Cargo.toml`；上游提交来自包内 `.cargo_vcs_info.json`。版本源码包是获取对应发布版本源码的入口，源码内的版权与许可证声明应予保留。

| 包及版本 | SPDX 许可证 | 对应源码 | 许可证来源 |
| --- | --- | --- | --- |
| `cssparser 0.36.0` | `MPL-2.0` | [版本源码包](https://crates.io/api/v1/crates/cssparser/0.36.0/download) · [上游提交](https://github.com/servo/rust-cssparser/tree/9339ddd71443463dfb85d1185ad50cd98b34bc8f) | [许可证全文](https://github.com/servo/rust-cssparser/blob/9339ddd71443463dfb85d1185ad50cd98b34bc8f/LICENSE) |
| `cssparser-macros 0.6.1` | `MPL-2.0` | [版本源码包](https://crates.io/api/v1/crates/cssparser-macros/0.6.1/download) · [上游提交](https://github.com/servo/rust-cssparser/tree/0ebd17bcd13808f087b083f34d2f816e380b5155/macros) | [许可证全文](https://github.com/servo/rust-cssparser/blob/0ebd17bcd13808f087b083f34d2f816e380b5155/LICENSE) |
| `dtoa-short 0.3.5` | `MPL-2.0` | [版本源码包](https://crates.io/api/v1/crates/dtoa-short/0.3.5/download) · [上游提交](https://github.com/upsuper/dtoa-short/tree/2d905cdb8b2e08163dc0d015f529877fe657b4ef) | [许可证全文](https://github.com/upsuper/dtoa-short/blob/2d905cdb8b2e08163dc0d015f529877fe657b4ef/LICENSE) |
| `option-ext 0.2.0` | `MPL-2.0` | [版本源码包](https://crates.io/api/v1/crates/option-ext/0.2.0/download) · [上游提交](https://github.com/soc/option-ext/tree/272f22fc9ea1ac6b08f01704af52c4ac338df4e2) | [许可证全文](https://github.com/soc/option-ext/blob/272f22fc9ea1ac6b08f01704af52c4ac338df4e2/LICENSE.txt) |
| `selectors 0.36.1` | `MPL-2.0` | [版本源码包](https://crates.io/api/v1/crates/selectors/0.36.1/download) · [上游提交](https://github.com/servo/stylo/tree/635e1a19d02960588a00e189bd4bd5bdb150ec3d/selectors) | [许可证声明（文件头）](https://github.com/servo/stylo/blob/635e1a19d02960588a00e189bd4bd5bdb150ec3d/selectors/lib.rs) |
| `foldhash 0.2.0` | `Zlib` | [版本源码包](https://crates.io/api/v1/crates/foldhash/0.2.0/download) · [上游提交](https://github.com/orlp/foldhash/tree/8f878c636fda9c9e93384824ea45e06d03f009f5) | [许可证全文](https://github.com/orlp/foldhash/blob/8f878c636fda9c9e93384824ea45e06d03f009f5/LICENSE) |
| `target-lexicon 0.12.16` | `Apache-2.0 WITH LLVM-exception` | [版本源码包](https://crates.io/api/v1/crates/target-lexicon/0.12.16/download) · [上游提交](https://github.com/bytecodealliance/target-lexicon/tree/7c80d459a9fdd121e9f23feb680c3db13c1baa39) | [许可证全文](https://github.com/bytecodealliance/target-lexicon/blob/7c80d459a9fdd121e9f23feb680c3db13c1baa39/LICENSE) |

`selectors 0.36.1` 的发布包没有独立 LICENSE 文件；其 `Cargo.toml` 和源码文件头声明 MPL-2.0，完整条款见 [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/)。`target-lexicon` 的 LICENSE 包含 Apache-2.0 正文与 LLVM Exceptions，不能省略例外部分。

分发提示：MPL-2.0 为文件级 copyleft，分发可执行形式时须告知接收者如何取得所覆盖源码，并按许可证提供对应源码（包括对覆盖文件的修改）。上述源码链接对应未修改的上游版本；若分发包含本地修改，必须另行提供匹配的修改后源码。保留各许可证要求的版权、许可及其他声明；链接索引不替代适用条款要求随制品提供的许可证副本或 NOTICE。发布方仍须检查其余依赖及最终制品的完整义务。
