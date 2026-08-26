# Round 1 提交清单（匿名化，供交叉验证）

## P-SEC 提交
- S1 [MEDIUM] web 请求解析器缺少 runtime/http.rs 同款加固：不拒 Transfer-Encoding、重复 Content-Length/Host 后者覆盖 | src/web/mod.rs:225-241 对照 http.rs:149-168 | 触发：TE+CL 同时发送双解析歧义
- S2 [MEDIUM] Origin 白名单 starts_with 前缀匹配放行形似源：http://127.0.0.1.evil.com 等 | src/web/mod.rs:169-173 | 触发：合法 CSRF token + 该 Origin → 200
- S3 [LOW] token 管线弱点：read_exact 错误吞掉可致全零 token；LCG 兜底熵弱；比较非常数时间；页面无 no-store | src/web/mod.rs:58,60-74,164,362-375

## P-RT 提交
- T1 [MEDIUM] failure_count 双计数：内层三分支先自增再因写失败返回 Err → 外层再自增 | src/runtime/mod.rs:552/561/582 与 :486 | 触发：客户端发一半即 RST
- T2 [MEDIUM] 拒绝路径在唯一 accept 循环内联执行：503 写无写超时；drain 上限按字节 2MiB 非时间——慢滴客户端可拖住 accept 数十秒 | src/runtime/mod.rs:457-463, http.rs:205-211
- T3 [LOW] 503 过载拒绝不计入任何统计指标 | src/runtime/mod.rs:456-464

## P-PROTO 提交
- C1 [MEDIUM] 重名拒绝只在 chat 入口生效：anthropic/responses 入口接受重名，且跨协议转 chat 时会重新发出重名工具 | chat.rs:287 vs anthropic.rs:520 / responses.rs:650 | 守卫应在共享 IR 校验层
- C2 [LOW] 新增 400 信封无诊断字段：InvalidRequest 静态文案，不如 Unsupported{field} 风格 | chat.rs:290, ir.rs:244
- C3 [LOW] 流式推理折叠进正文无标记且增量无分隔符，非流式用 \n 连接——多轮 agent 会把 CoT 存为答案历史 | chat.rs:918 vs 611-624

## P-FE 提交
- E1 [HIGH] 外来 Origin 测试未隔离 Origin 防线：请求没带 csrf 头（403 来自 token 分支）、还指错端口（_port2 未用）；删掉整个 Origin 分支测试仍全绿 | src/web/mod.rs:489-493 vs 165-175
- E2 [MEDIUM] 同 S2（Origin 前缀匹配）— 独立命中
- E3 [LOW] renderPanel() 不返回 promise，await 成空操作 → pending_open 陈旧状态竞态 | app.js:442-445 consumed at 188/298

## 去重
S2 ≡ E2（双人独立命中，各享 ×1.1）。共 11 条唯一发现。
