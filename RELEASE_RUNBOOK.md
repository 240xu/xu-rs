# 发版手册（GitHub Release + npm）

> 适用：`@240xu/trivium` npm 包 + `240xu/xu-rs` GitHub Release（android-aarch64 预编译二进制）。
> 全流程可在本机（Termux）完成；最后一次实测：npm `0.1.3` × Release `v0.1.1`（2026-09-17）。

## 版本三层解耦（不要混淆）

| 层 | 当前值 | 在哪定义 | 说明 |
|---|---|---|---|
| 二进制自报版本 | `0.1.0`（`trivium --version`） | `Cargo.toml` | 未随补丁线 bump，仅作身份标识 |
| Release tag / BIN_VERSION | `v0.1.1` / `"0.1.1"` | git tag + `npm/install.js` 的 `BIN_VERSION` | 决定下载哪个 Release 资产 |
| npm 包版本 | `0.1.3` | `npm/package.json` | 安装器逻辑变更时 bump；可独立于二进制重发 |

`install.js` 注释明确了 BIN_VERSION 与 npm VERSION 的解耦：装器修复可以单独重发。

## 资产契约（打错必炸，E2E 会拦）

1. **tarball 顶层目录 = `trivium-<BIN_VERSION>-android-aarch64`**，内含 `bin/{trivium,xcc,spec}` 三个可执行（同二进制三份拷贝；本机文件系统禁硬链接，无法用 ln 省体积）。
2. **资产文件名** = `trivium-<BIN_VERSION>-android-aarch64.r<N>.tar.gz` —— `.r<N>` 后缀是 **CDN 缓存破坏位**：GitHub 对同名 Release 资产有边缘缓存，删除重建同名资产后直链仍可能返回旧字节（size 守卫会拦下），此时必须递增 `.rN`。
3. **tar 内部路径与资产名解耦**：`install.js` 用固定常量 `trivium-<BIN_VERSION>-android-aarch64` 作为抽取前缀，资产名只用于下载。
4. `install.js` 必须同步四个常量：`BIN_VERSION`、`CHECKSUMS[资产名]`、`ASSET_ID`（数字 id，来自 REST API，**不是** `RA_kw…` 的 GraphQL node id）、`EXPECTED_SIZE`。

## 发版步骤（实测顺序）

```sh
# 0) 前置：cargo build --release 与待发布提交一致；gh 已登录；npm 已登录（npm whoami）

# 1) 组装 tarball（注意顶层目录名）
REL=<stage-dir>
mkdir -p $REL/trivium-0.1.1-android-aarch64/bin
cp -p target/release/trivium $REL/trivium-0.1.1-android-aarch64/bin/trivium
cp -p $REL/trivium-0.1.1-android-aarch64/bin/trivium $REL/trivium-0.1.1-android-aarch64/bin/xcc
cp -p $REL/trivium-0.1.1-android-aarch64/bin/trivium $REL/trivium-0.1.1-android-aarch64/bin/spec
cd $REL && tar -czf trivium-0.1.1-android-aarch64.r2.tar.gz trivium-0.1.1-android-aarch64

# 2) 校验和 + 大小
sha256sum trivium-0.1.1-android-aarch64.r2.tar.gz | cut -d' ' -f1
stat -c %s     trivium-0.1.1-android-aarch64.r2.tar.gz

# 3) 抽取烟测（三入口 + --version）
tar -tzf <tarball>                       # 三条 bin/ 项齐全
tar -xzf <tarball> -C <tmp> --strip-components=2 trivium-0.1.1-android-aarch64/bin/{trivium,xcc,spec}
<tmp>/trivium --version

# 4) 发 Release（首次；替换资产用 gh release upload --clobber，同名无效则删资产重传+递增 .rN）
gh release create v0.1.1 <tarball> SHA256SUMS --title … --notes …

# 5) 取数字 asset id 并写回 install.js
gh api repos/240xu/xu-rs/releases/tags/v0.1.1 --jq '.assets[] | {name,id,size}'

# 6) E2E（真实下载链路，必须过）
cd npm && npm pack
TP=<tmp>; npm install --prefix $TP <npm/240xu-trivium-x.y.z.tgz>
$TP/node_modules/@240xu/trivium/vendor/trivium doctor   # 应输出补丁清单
head -1 $TP/node_modules/@240xu/trivium/bin/trivium.js  # Android 上应为绝对 node shebang

# 7) 提交推送 + 发布
git commit / push
cd npm && npm publish --access public
npm view @240xu/trivium version    # 等 registry 传播（实测 ~20s）
```

## 已知坑（全部踩过）

| 坑 | 现象 | 处置 |
|---|---|---|
| tar 顶层目录名 ≠ 资产名去后缀 | 安装期 `tar: …/bin/spec: Not found in archive` | 按上面契约命名（install.js 的 `base` 常量已与资产名解耦，改资产名不再影响内部路径） |
| 同名替换 Release 资产 | 下载拿到旧字节（size 守卫拦截） | 资产名递增 `.rN` 重新上传 |
| `gh release upload --clobber` 静默不生效 | 资产列表 size 未变 | 删资产（`gh api -X DELETE …/releases/assets/<id>`）再上传 |
| 新版 npm 门控 postinstall | `npm warn install-scripts`，vendor 未下载 | `npm config set ignore-scripts false` 或 `npm install-scripts approve @240xu/trivium` |
| 本机文件系统禁硬链接 | `ln` 报 Permission denied | bin 三入口用真实拷贝（体积 ×3，可接受） |
| 后台 `cargo build` 跑错 workdir | 构建失败还部署了旧二进制 | 构建必须前台/显式 workdir；部署前比对 `sha256sum` old≠new 才 cp |

## git push 断网降级：Git Data API

本机代理对 `github.com:443` 的路由可能死亡（`api.github.com` 通常存活）。此时 `git push` 报 `TLS connect error: unexpected eof`，改走 REST 完成推送：

```sh
# blob → tree（base_tree 基于远端当前 tip 的 tree）→ commit（parent=远端 tip）→ PATCH ref
gh api repos/OWNER/REPO/git/blobs   -X POST --input -   # {content(base64), encoding}
gh api repos/OWNER/REPO/git/trees   -X POST --input -   # {base_tree, tree:[{path,mode,type,sha}]}
gh api repos/OWNER/REPO/git/commits -X POST --input -   # {message, tree, parents}
gh api repos/OWNER/REPO/git/refs/heads/<branch> -X PATCH --input -   # {sha, force}
```

教训两条：
1. **API 返回的 commit sha 必须完整记录**（截断后凭记忆补全是 422 "Object does not exist" 的直接原因；未引用对象无法用短 sha 反查）。
2. API 创建的 commit 时间戳与本地不同 → sha 不同（内容一致）。网络恢复后 `git fetch && git reset --soft origin/<branch>` 对齐。

## 检查清单（发版前过一遍）

- [ ] `cargo test` 全绿；`rustfmt --check` 干净
- [ ] 二进制构建自**待发布提交**（`git log -1` 与构建时间核对，防旧产物）
- [ ] tarball 三入口齐全、抽取烟测 `--version` 通过
- [ ] install.js 四常量与实际资产一致（`node --check` 过）
- [ ] E2E：临时 prefix 真下载安装，`doctor` 输出补丁清单
- [ ] 泄密扫描：`git diff origin/main..HEAD` 无密钥/内部域名/IP（公开仓）
- [ ] CHANGELOG 有对应版本节，描述与实际二进制一致
