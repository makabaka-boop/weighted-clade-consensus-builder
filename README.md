# clades

无依赖 Rust JSON 命令行工具，用于从多棵带权有根二叉系统发育树构造贪心加权共识树。

## 输入

从文件参数读取，或在无参数时从标准输入读取：

```bash
clades request.json
clades < request.json
```

请求必须是 JSON 对象：

```json
{
  "leaves": ["A", "B", "C", "D"],
  "trees": [
    [[["A", "B"], "C"], "D"],
    [["A", "B"], ["C", "D"]]
  ],
  "weights": [2, 3]
}
```

约束：

- `leaves`：3～20 个唯一、非空、可打印 ASCII 叶名。
- `trees`：2～30 棵有根二叉树。
- 叶节点是字符串；内部节点是恰有两个元素的数组。
- 每棵树必须恰好包含全体叶子一次。
- `weights`：与树一一对应的整数，范围 1～10。

## 算法

1. 提取每棵树除根集合和单叶集合以外的全部内部叶集合。
2. 对每个集合累加包含该集合的树权重，得到加权支持度。
3. 排序规则依次为：支持度降序、集合大小升序、叶名列表字典序。
4. 按顺序接纳与所有已选集合相容的集合；两个集合相交且互不包含即冲突。
5. 对拒绝的集合记录遍历已选集合时最先遇到的冲突见证。
6. 根据选中集合重建允许多叉的有根共识树，子节点按最小叶名排序。
7. 支持率写为约分后的加权支持度 / 总权重。

## 输出

输出为 JSON 对象。`groups` 保留全部候选分组及接纳状态；被拒绝分组的
`first_conflict_with` 给出首个冲突的已选分组；`consensus` 是重建的共识树。

## Compose

Compose 的 `clades` 服务可直接构建仓库内的 `Dockerfile`。镜像入口点为
`clades`，通过标准输入接收 JSON 并向标准输出写结果：

```yaml
services:
  clades:
    build: .
    stdin_open: true
```

## 开发

```bash
cargo test
cargo build --release
./target/release/clades request.json
```
