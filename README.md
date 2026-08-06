# Schemadex

本地 `.litematic` 蓝图库管理器。**不开游戏就能翻自己的蓝图。**

只做两件事：

1. **看蓝图长什么样** —— 等距缩略图 + 3D 可旋转预览 + Y 轴逐层切片
2. **跨蓝图材料汇总** —— 多选蓝图，合并出一份「盒 / 组 / 个」的备货清单

为什么只做这两件：生电生态里 Litematica、MiniHUD、Tweakeroo、RSMM、
Item Flow Monitor、techutils 已经把投影、材料清单、仓库对账、HUD、红石时序、
农场速率都覆盖了。没人做的是**本地蓝图库级管理**——在线 viewer 一次只能拖一个文件看，
而硬盘上那两千个 `iron_farm_v3_final_真final.litematic` 只能靠回忆去找。

> 非官方工具，与 Mojang / Microsoft 无关。

## 装上就能用

1. 从 [Releases](../../releases) 下载安装包，装好打开。
2. **首次启动会让你指一个 Minecraft 客户端 jar**（一般在
   `.minecraft/versions/<版本>/<版本>.jar`），程序会从里面提取一份贴图和模型，
   十几秒，只做一次。自动找不到就手动选。
3. 指定蓝图目录。用 PCL / HMCL 且开了版本隔离的话，每个
   `versions/<版本>/schematics/` 下都有一份，程序会自动把它们都找出来。
4. 点扫描。

同一个蓝图在多个版本目录各存一份是常态（实测 64% 的文件是副本），
去重按内容哈希，重复的自动合并成一条。

## 关于 Minecraft 素材

**本项目不包含、也不分发任何 Minecraft 素材。**

画蓝图需要方块贴图、模型和中文译名，这些都是 Mojang 的资源。Schemadex 的做法是
在你自己的电脑上、从你自己已经装好的客户端 jar 里提取一份，存到本机的应用数据目录。
程序本身、以及 Releases 里的安装包，都不带这些文件。

所以：**你需要本机装有 Minecraft**。对一个 MC 蓝图工具来说这不算额外要求。

## 命令行

`schemadex` 是同一套内核的 CLI，也是这个项目的正确性验证入口。

```bash
schemadex scan   <dir>                                  # 扫描目录并按内容去重
schemadex info   <file.litematic>                       # 单个蓝图的结构信息
schemadex mats   <file...>                              # 材料清单，多文件自动汇总
schemadex verify <dir>                                  # 全量对拍：实算值 vs Litematica 声明值
schemadex colors <client.jar> <out.json>                # 从客户端 jar 提取材质表
schemadex thumb  <file> <colors.json> <out.png>         # 等距缩略图
schemadex slice  <file> <colors.json> <y> <out.png>     # 第 y 层俯视切片
schemadex voxels <file> <colors.json> [max_grid]        # 表面体素统计（3D 预览的数据源）
schemadex sample <colors.json> <out.png> <block...>     # 方块对照表，可带方块状态
```

`verify` 拿 `Metadata.TotalBlocks`（Litematica 自己数出来的非空气方块数）当标准答案——
位解包、调色板顺序、空气判定错一处这个数就对不上。它会自己判定通过与否并给出退出码。
当前在 2138 个真实蓝图上 **2132 一致、0 解析失败**；剩下 6 个是蓝图作者把 Metadata
改成了梗数字（`1919810` / `114514`），已按内容哈希登记为已知例外。

## 从源码构建

需要 Rust（MSVC 工具链）和 Node。

```bash
npm install
npm run tauri build
```

产物是安装包；想要免安装的 exe 就加 `-- --no-bundle`，出在
`target/release/schemadex-app.exe`。

> ⚠️ **别用 `cargo build -p schemadex-app`。** 不经 Tauri CLI 编出来的是 dev 模式二进制，
> 前端资源没被内嵌，启动后会去连 `devUrl`（localhost:1421），窗口里显示
> 「无法访问此页面」。为此 `src-tauri` 已被排除出 workspace 的 `default-members`。
> 判断手上的 exe 是哪种：生产构建里能搜到 `/assets/index-` 字符串。

命令行工具不受此限制：

```bash
cargo build --release
```

## 结构

| crate | 职责 |
| --- | --- |
| `crates/litematic` | `.litematic` 解析：流式 NBT、跨 long 位解包、材料映射 |
| `crates/mcassets` | 从客户端 jar 提取模型 / 材质图集 / 中文名 |
| `crates/render` | 等距缩略图、Y 轴切片、体素模型 |
| `crates/schemadex-cli` | 命令行，也是正确性验证入口 |
| `src-tauri` + `src` | 桌面应用（Tauri 2 + React + three.js） |

## 想改代码？先读 HANDOFF

[`HANDOFF.md`](HANDOFF.md) 是这个项目的交割文档，讲「为什么长这样、坑在哪」：

- `.litematic` 格式在两千个真实文件上实测出来的硬约束（跨 long 打包、负 `Size`、
  被篡改的 Metadata、5 亿方块的巨物……）
- 材质与模型踩过的十一个坑，每一条都对应一次「渲染出来不对」
- 三条渲染路径各自的验证方法——改解析跑 `verify`，改渲染出 `sample` 对照表，
  改着色器跑 `tools/rendercheck.html` 做像素级比对
- **渲染改动的取舍标准**：这是缩略图工具，不是 MC 建模器，
  判据是「不修的话会认错方块吗」

这个项目在渲染上返工过五轮，每一轮都是因为「看着差不多」就交付了。上面那些验证入口
都是现成的，跑一次都不到一分钟。

## 已知取舍

- 材料清单里方块 → 物品的映射是手工表（离线拿不到游戏的 `getCloneItemStack`）。
  已知不精确的项（如蜡烛蛋糕只算蛋糕）会标 `~` 提示复核。
- 水 / 岩浆默认不算桶。含水方块很容易让数字虚高，需要时在界面上勾选。
- 「容器内已有物品」单独一栏，不混进材料清单——那是分类系统的样板物品、
  漏斗计时器的填充物，不是建造材料。
- 模型的 element 级旋转没做。墙上火把不是斜的、拉杆手柄是直立的——
  不影响认出这是什么方块，按上面那条取舍标准就不做。
- 自动探测 `.minecraft` 目前只覆盖 Windows。其它平台手动指定目录即可。

## 许可

代码 [MIT](LICENSE)。许可只覆盖本项目自己的源码，不涉及 Minecraft 素材（见 [NOTICE.md](NOTICE.md)）——
那些是在你本机从你自己的游戏里提取的，权利归 Mojang，适用 Minecraft EULA。
