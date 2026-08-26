# 盲评积分与奖励机制 v1

## 角色
- 选手（player）：领域评审子代理，产出带锚点 findings + 自评分。
- 裁判（judge）：交叉验证子代理，对同一批 findings 逐条判定 CONFIRMED / PARTIAL / REFUTED，
  并可提交「遗漏发现」赚额外积分。裁判互不知晓选手身份与自评分。
- 终裁（arbiter）：主代理。持验收标准；分歧票由终裁亲自读码裁决。

## 单条发现计分
value = severity_weight × validity_factor
- severity_weight：CRITICAL=60 · HIGH=40 · MEDIUM=25 · LOW=10（以裁判修正后严重度为准）
- validity_factor：CONFIRMED=1.0 · PARTIAL=0.5 · REFUTED=0
- 双人独立命中同一问题：各按全值 ×1.1（独立性加成）
- 裁判遗漏发现：按同公式入账，×1.2（新信息加成）

## 选手轮次综合分（0-100）
composite = min(100, raw_points + accuracy×20 − refuted_count×5)
- accuracy = CONFIRMED 条数 / 报告总条数
- REFUTED 含锚点缺失、不可复现、断言错误

## 奖励与激励闭环
1. 榜首（composite 最高）下轮升任「首席评审」：其 LOW 级发现免验证直采。
2. accuracy < 0.5 的选手下轮缩域（只给单文件），连续两轮 <0.5 除名。
3. 每条 CONFIRMED 发现自动转化为修复指令（扣分明细即下轮 TODO，F1 激励闭环）。
4. REFUTED 判定附理由回传对应选手上下文作校准反馈。
5. 全员 composite 与榜单持久化于本目录 leaderboard.md。

## 收敛门禁（终裁持有，选手不得自定）
- 四域均 ≥8.5/10 且无未修 CONFIRMED CRITICAL/HIGH ⇒ 通过。
- 否则打回修复 → 复评 Δ，最多 3 轮；3 轮未收敛 ⇒ STOP 并上报残余风险。
