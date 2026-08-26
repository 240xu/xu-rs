# Round 1 终裁裁决表（arbiter: 主代理）

| ID | J1 | J2 | 终裁 | 理由（亲读码） |
|---|---|---|---|---|
| S1 | PARTIAL/LOW | PARTIAL/LOW | **PARTIAL/LOW** | 加固差距属实但 loopback+connection: close 无走私前置；仍修（对齐 http.rs） |
| S2 | PARTIAL/LOW | CONFIRMED/MED | **CONFIRMED/MEDIUM** | 前缀匹配确证放行形似源；防线失其声明职责即为缺陷，可利用性只影响分不加权 |
| S3 | CONFIRMED/LOW | CONFIRMED/LOW | **CONFIRMED/LOW** | 四弱点全实；捆绑修复 |
| T1 | CONFIRMED/MED | CONFIRMED/MED | **CONFIRMED/MEDIUM** | 三分支先增后 Err、外层再增——双计数实锤 |
| T2 | CONFIRMED/MED | PARTIAL/LOW | **PARTIAL/LOW** | drain 半边被驳（nonblocking WouldBlock 即返，J2 对）；存活半边=accept 内联写无写超时，修 |
| T3 | CONFIRMED/LOW | CONFIRMED/LOW | **CONFIRMED/LOW** | 属实；加 rejected_count |
| C1 | CONFIRMED/MED | CONFIRMED/MED | **CONFIRMED/MEDIUM** | 守卫错层：共享 IR 校验缺失，跨协议重名穿透 |
| C2 | PARTIAL/LOW | CONFIRMED/LOW | **CONFIRMED/LOW** | 信封零诊断属实；换 Unsupported{field} |
| C3 | CONFIRMED/LOW | CONFIRMED/LOW | **CONFIRMED/LOW · 缓修** | 改折叠语义有客户端破坏风险；记已知限制 |
| E1 | CONFIRMED/HIGH | PARTIAL/MED | **CONFIRMED/MEDIUM** | 事实三点全实（无头/错端口/分支可删）；测试缺口随被掩控制定级 |
| E2 | PARTIAL/LOW | CONFIRMED/MED | **≡S2 CONFIRMED/MEDIUM** | 独立命中成立 |
| E3 | PARTIAL/LOW | PARTIAL/LOW | **PARTIAL/LOW** | 机制真、路径不可达；一行加固照修 |

## 裁判遗漏发现采纳
- JM-spawn [MEDIUM·J2] thread::spawn EAGAIN panic 杀死守护循环 → 采纳修复（双 serve 循环 catch_unwind）
- JM-ipv6 [LOW·J2] host_is_local 拒绝浏览器真实形态 `[::1]:port` → 采纳修复（防线反转）
- JM-health [LOW·J1] /health 自 poll 计入 success 污染指标 → 采纳（并入记账枚举重构）
- JM-date [LOW·J2] 缺 Date 头（RFC 9110 §6.6.1）→ 采纳（JSON/text/raw 响应）
- JM-webbound [LOW·J1] web 无并发界/O(n²) 重扫 → **缓修**（低流量控制台，重构成本>收益，记录）
- JM-shutdown [LOW·J1] drain 无上限无强杀 → **缓修**（既有遗留项，记录）

## 积分榜（severity_weight×validity，独立命中×1.1）
| 选手 | raw | accuracy | composite | 名次 |
|---|---|---|---|---|
| P-FE | 57.5 | 2/3 | **70.8** | 🥇 首席评审(R2) |
| P-PROTO | 45.0 | 3/3 | **65.0** | 🥈 |
| P-SEC | 42.5 | 2/3 | **55.8** | 🥉 |
| P-RT | 40.0 | 2/3 | **53.3** | 4 |
| J2(遗漏发现) | 54.0(×1.2) | — | 54.0 | 最佳裁判 |
| J1(遗漏发现) | 36.0(×1.2) | — | 36.0 | |

全员 accuracy ≥0.5，无人缩域。CONFIRMED 共 9 条 → 全部转为本轮修复指令。
