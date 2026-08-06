//! litevault CLI —— 在接 UI 之前先把解析正确性验穿。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

use litematic::materials::{stack_size, to_stacks, MaterialOptions};
use litematic::{LoadMode, MaterialList, Schematic};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "用法:\n  \
             litevault info     <file.litematic>       单个蓝图的结构信息\n  \
             litevault verify   <dir>                  全量对拍：实算值 vs Litematica 声明值\n  \
             litevault mats     <file...>              材料清单（多文件自动汇总）\n  \
             litevault scan     <dir>                  扫描目录并按内容去重\n  \
             litevault colors   <client.jar> <out.json> 从游戏 jar 提取方块颜色表\n  \
             litevault thumb    <file> <colors.json> <out.png>        等距缩略图\n  \
             litevault slice    <file> <colors.json> <y> <out.png>    第 y 层俯视切片\n  \
             litevault voxels   <file> <colors.json> [max_grid]       表面体素统计（3D 预览的数据源）\n  \
             litevault sample   <colors.json> <out.png> <block[prop=v,...]...>  对照表，可带方块状态"
        );
        std::process::exit(2);
    }
    match args[0].as_str() {
        "info" => cmd_info(Path::new(&args[1])),
        "verify" => cmd_verify(Path::new(&args[1])),
        "mats" => cmd_mats(&args[1..].iter().map(PathBuf::from).collect::<Vec<_>>()),
        "scan" => cmd_scan(Path::new(&args[1])),
        "colors" => cmd_colors(Path::new(&args[1]), Path::new(&args[2])),
        "thumb" => cmd_thumb(Path::new(&args[1]), Path::new(&args[2]), Path::new(&args[3])),
        "slice" => cmd_slice(
            Path::new(&args[1]),
            Path::new(&args[2]),
            args[3].parse()?,
            Path::new(&args[4]),
        ),
        "sample" => cmd_sample(
            Path::new(&args[1]),
            Path::new(&args[2]),
            &args[3..].iter().map(String::as_str).collect::<Vec<_>>(),
        ),
        "voxels" => cmd_voxels(
            Path::new(&args[1]),
            Path::new(&args[2]),
            args.get(3).and_then(|s| s.parse().ok()).unwrap_or(256),
        ),
        other => bail!("未知子命令 {other}"),
    }
}

/// 读材质表。图集 PNG 是同名同目录的 `.png`，一并加载。
fn load_colors(path: &Path) -> Result<(mcassets::BlockAssets, Option<render::Atlas>)> {
    let s = fs::read_to_string(path)
        .with_context(|| format!("读不到材质表 {}（先跑 litevault colors 生成）", path.display()))?;
    let assets: mcassets::BlockAssets = serde_json::from_str(&s)?;
    let png = path.with_extension("png");
    let atlas = match fs::read(&png) {
        Ok(bytes) => Some(render::Atlas::from_png(
            &bytes,
            assets.tile_size.max(1),
            assets.tiles_per_row.max(1),
        )?),
        Err(_) => {
            eprintln!("提示: 没找到图集 {}，退回平均色渲染", png.display());
            None
        }
    };
    Ok((assets, atlas))
}

fn cmd_colors(jar: &Path, out: &Path) -> Result<()> {
    let t0 = std::time::Instant::now();
    let extracted = mcassets::extract(jar)?;
    let colors = &extracted.assets;
    // 紧凑序列化：模型库比原来的颜色表大一个数量级，pretty 会再涨一倍多
    fs::write(out, serde_json::to_string(colors)?)?;
    let png = out.with_extension("png");
    fs::write(&png, &extracted.atlas_png)?;
    println!(
        "游戏版本 {} → {} 个方块，{} 个模型，{} 条中文名，{} 个解析不出材质，耗时 {:.2}s",
        colors.version,
        colors.blocks.len(),
        colors.models.len(),
        colors.names.len(),
        colors.unresolved.len(),
        t0.elapsed().as_secs_f64()
    );
    println!(
        "图集: {} 个图块 {}×{} 每块，{}×{} 像素，PNG {:.0} KB → {}",
        colors.tile_count,
        colors.tile_size,
        colors.tile_size,
        colors.tiles_per_row * colors.tile_size,
        colors.tiles_per_row * colors.tile_size,
        extracted.atlas_png.len() as f64 / 1024.0,
        png.display()
    );
    if !colors.unresolved.is_empty() {
        println!("解析不出的（前 20 个）:");
        for b in colors.unresolved.iter().take(20) {
            println!("  {b}");
        }
    }
    // 抽查几个容易错的：楼梯该用木板色、草方块该是绿的、红石线该是红的
    // 抽查几个有代表性的状态，确认解析结果的形状数与朝向对得上
    println!("\n抽查（方块状态 → 解析出的长方体数）:");
    for (b, props) in [
        ("minecraft:stone", ""),
        ("minecraft:oak_stairs", "facing=east,half=bottom,shape=straight"),
        ("minecraft:oak_slab", "type=bottom"),
        ("minecraft:oak_slab", "type=double"),
        ("minecraft:oak_trapdoor", "facing=north,half=top,open=false"),
        ("minecraft:piston", "extended=false,facing=east"),
        ("minecraft:ladder", "facing=south"),
        ("minecraft:oak_fence", "north=true,south=true,east=false,west=false"),
        ("minecraft:chest", ""),
    ] {
        let p = parse_props(props);
        let boxes = colors.resolve(b, &p);
        let name = colors.name_of(b).unwrap_or("-");
        println!(
            "  {:<24} {:<40} {} 个长方体  {}",
            name,
            format!("{b}[{props}]"),
            boxes.len(),
            boxes
                .first()
                .map_or("（无模型，走平均色）".to_string(), |x| format!("首个 {:?}", x.bbox))
        );
    }
    println!("\n写入 {}", out.display());
    Ok(())
}

fn cmd_thumb(file: &Path, colors_path: &Path, out: &Path) -> Result<()> {
    let (colors, atlas) = load_colors(colors_path)?;
    let t0 = std::time::Instant::now();
    let schem = Schematic::load(file, LoadMode::Full)?;
    let load_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let opts = render::RenderOptions { background: None, ..Default::default() };
    let grid = render::VoxelGrid::build(&schem, &colors, atlas.as_ref(), &opts)
        .ok_or_else(|| anyhow::anyhow!("蓝图没有 region"))?;
    let img = render::isometric_grid(&grid, atlas.as_ref(), &opts);
    img.save(out)?;

    let (lo, hi) = schem.bounding_box().unwrap();
    println!(
        "{}\n  包围盒 {}×{}×{}  体积 {}\n  网格 {}×{}×{} (1格={}³方块)  实心格 {}\n  输出 {}×{} → {}\n  解析 {}ms  渲染 {}ms",
        name(file),
        hi.x - lo.x + 1, hi.y - lo.y + 1, hi.z - lo.z + 1, schem.total_volume(),
        grid.w, grid.h, grid.d, grid.scale, grid.solid_count(),
        img.width(), img.height(), out.display(),
        load_ms, t1.elapsed().as_millis()
    );
    Ok(())
}

fn cmd_slice(file: &Path, colors_path: &Path, y: usize, out: &Path) -> Result<()> {
    let (colors, atlas) = load_colors(colors_path)?;
    let schem = Schematic::load(file, LoadMode::Full)?;
    let opts = render::RenderOptions {
        background: Some([0x1a, 0x1a, 0x1a]),
        ..Default::default()
    };
    // 格子大小按横向尺寸自适应：写死 8 的话 1424×602 的蓝图会生成
    // 11392×4816 的图，光位图就 220 MB
    let (lo, hi) = schem.bounding_box().unwrap_or_default();
    let widest = (hi.x - lo.x + 1).max(hi.z - lo.z + 1).max(1) as u32;
    let cell_px = (900 / widest).clamp(2, 24);
    let img = render::slice_top_down(&schem, &colors, atlas.as_ref(), y, cell_px, &opts)?;
    img.save(out)?;
    println!(
        "{} 第 {y} 层 → {}×{} 写入 {}",
        name(file),
        img.width(),
        img.height(),
        out.display()
    );
    let counts = render::layer_counts(&schem).unwrap_or_default();
    let max = counts.iter().copied().max().unwrap_or(1).max(1);
    println!("共 {} 层，每层非空气方块数:", counts.len());
    for (ly, n) in counts.iter().enumerate() {
        let bar = "#".repeat((*n * 40 / max) as usize);
        println!("  y={ly:<3} {n:>9}  {bar}");
    }
    Ok(())
}

fn collect(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("litematic")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn cmd_info(path: &Path) -> Result<()> {
    let s = Schematic::load(path, LoadMode::Full)?;
    println!("文件      : {}", path.display());
    println!("格式版本  : Version={} SubVersion={:?}", s.version, s.sub_version);
    println!("数据版本  : {}", s.data_version);
    println!("名称      : {:?}", s.metadata.name);
    println!("作者      : {:?}", s.metadata.author);
    println!("region 数 : {}", s.regions.len());

    let vol = s.total_volume();
    let blocks = s.total_blocks().unwrap_or(0);
    println!("\n实算体积  : {vol}");
    println!("实算方块  : {blocks}  (非空气)");
    match (s.metadata.declared_total_volume, s.metadata.declared_total_blocks) {
        (Some(dv), Some(db)) => {
            println!(
                "声明体积  : {dv}  {}",
                if dv as u64 == vol { "✓" } else { "✗ 声明值不可信" }
            );
            println!(
                "声明方块  : {db}  {}",
                if db as u64 == blocks { "✓" } else { "✗ 声明值不可信" }
            );
        }
        _ => println!("(Metadata 未声明统计值)"),
    }

    if let Some((lo, hi)) = s.bounding_box() {
        println!(
            "\n包围盒    : ({},{},{}) .. ({},{},{})  尺寸 {}×{}×{}",
            lo.x, lo.y, lo.z, hi.x, hi.y, hi.z,
            hi.x - lo.x + 1, hi.y - lo.y + 1, hi.z - lo.z + 1
        );
    }

    println!("\n各 region:");
    for r in &s.regions {
        let e = r.extent();
        println!(
            "  {:<24} pos=({:>5},{:>4},{:>5}) size=({:>5},{:>4},{:>5}) -> {}×{}×{}  调色板 {:>5}  bits {:>2}  {}",
            r.name, r.position.x, r.position.y, r.position.z,
            r.size.x, r.size.y, r.size.z, e.x, e.y, e.z,
            r.palette.len(), r.bits(),
            if r.is_consistent() { "自洽" } else { "*** 长度不自洽 ***" }
        );
        if !r.tile_entities.is_empty() {
            let with_items = r.tile_entities.iter().filter(|t| !t.items.is_empty()).count();
            let no_id = r.tile_entities.iter().filter(|t| t.id.is_none()).count();
            println!(
                "    方块实体 {} 个（{} 个带物品，{} 个无 id 字段），实体 {} 个",
                r.tile_entities.len(), with_items, no_id, r.entity_count
            );
        }
    }
    Ok(())
}

/// 全量对拍。
///
/// Litematica 写进 Metadata 的 `TotalBlocks` 是它自己数出来的非空气方块数——
/// 这是现成的标准答案。位解包、调色板顺序、空气判定只要错一处，这个数就对不上。
fn cmd_verify(dir: &Path) -> Result<()> {
    let files = collect(dir);
    println!("对拍 {} 个文件…\n", files.len());

    let done = AtomicUsize::new(0);
    let total = files.len();
    let results: Vec<(PathBuf, VerifyOutcome)> = files
        .par_iter()
        .map(|p| {
            let r = verify_one(p);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 200 == 0 {
                eprintln!("  … {n}/{total}");
            }
            (p.clone(), r)
        })
        .collect();

    let mut ok = 0usize;
    let mut no_decl = 0usize;
    let mut failed = Vec::new();
    let mut inconsistent = Vec::new();
    // 不一致的文件：(路径, 内容哈希, 声明体积, 实算体积, 声明方块数, 实算方块数)
    let mut mismatch: Vec<(&PathBuf, String, i64, u64, i64, u64)> = Vec::new();

    for (p, r) in &results {
        match r {
            VerifyOutcome::Failed(e) => failed.push((p, e.clone())),
            VerifyOutcome::NoDeclaration => no_decl += 1,
            VerifyOutcome::Checked { vol_ok, blk_ok, declared_vol, actual_vol, declared_blk, actual_blk, all_consistent } => {
                if !all_consistent {
                    inconsistent.push(p);
                }
                if *vol_ok && *blk_ok {
                    ok += 1;
                } else {
                    mismatch.push((
                        p,
                        hash_file(p),
                        *declared_vol,
                        *actual_vol,
                        *declared_blk,
                        *actual_blk,
                    ));
                }
            }
        }
    }

    // 已知例外 vs 新出现的——只有后者才说明解析出了问题
    let (known, fresh): (Vec<_>, Vec<_>) =
        mismatch.iter().partition(|(_, h, ..)| known_lie(h).is_some());

    println!("\n================ 对拍结果 ================");
    println!("完全一致          : {ok} / {total}");
    println!("Metadata 未声明   : {no_decl}");
    println!("解析失败          : {}", failed.len());
    println!("BlockStates 不自洽: {}", inconsistent.len());
    println!("已知例外          : {} （Metadata 被作者改成了梗数字）", known.len());
    println!("**新出现的不一致**: {}", fresh.len());

    if !failed.is_empty() {
        println!("\n--- 解析失败 ---");
        for (p, e) in failed.iter().take(10) {
            println!("  {}\n    {e}", name(p));
        }
    }
    if !known.is_empty() {
        println!("\n--- 已知例外（不是 bug）---");
        for (p, h, dv, av, db, ab) in known.iter().take(20) {
            println!(
                "  {}\n    体积 {dv} vs {av}  方块 {db} vs {ab}  [{}]",
                name(p),
                known_lie(h).unwrap_or("")
            );
        }
    }
    if !fresh.is_empty() {
        println!("\n--- 新出现的不一致 ← 这才是解析 bug 的信号 ---");
        for (p, h, dv, av, db, ab) in fresh.iter().take(20) {
            println!("  {}", name(p));
            println!("    体积 声明={dv:<12} 实算={av:<12} 差={:+}", *av as i64 - dv);
            println!("    方块 声明={db:<12} 实算={ab:<12} 差={:+}", *ab as i64 - db);
            // 确认过是 Metadata 被篡改的话，把这行填进 KNOWN_META_LIES
            println!("    确认非 bug 后加入名单: (\"{h}\", \"说明\"),");
        }
    }

    println!("\n================================================");
    if fresh.is_empty() && failed.is_empty() {
        println!("通过：没有新出现的不一致。");
        Ok(())
    } else {
        // 非零退出码，这样它能直接当回归测试用
        bail!(
            "对拍未通过：{} 个新出现的不一致，{} 个解析失败",
            fresh.len(),
            failed.len()
        )
    }
}

/// **已知例外**：Metadata 被蓝图作者改成了梗数字（`1919810` / `114514` /
/// `20060210`），不是解析 bug。按 blake3 内容哈希认人——同一份蓝图在 6 个版本
/// 目录里各有副本，按路径记名单会漏。
///
/// 名单存在的意义：让 `verify` 自己判定通过与否。以前只印一句
/// 「完全一致 2132 / 2138」，得靠人记住 2132 这个数才知道有没有退步；
/// 悄悄掉到 2130 没人会发现。**新出现的不一致才是解析 bug 的信号。**
///
/// 加新条目前先确认它真的是 Metadata 被篡改，而不是解析错了。
/// 4 个哈希覆盖 6 个文件——其中一个在三个版本目录里各有一份副本。
const KNOWN_META_LIES: &[(&str, &str)] = &[
    // 六个全都声明 TotalVolume=1919810、TotalBlocks=114514。
    // 有两个的实算体积比声明值还大（2594592 > 1919810），
    // 单这一条就说明声明值是假的：解析错了只会少数、不会凭空多出方块。
    (
        "bd0be6057fb28825dabb7ce23e0cd83fa1838700e3924282715b3f75cc48857d",
        "火弦月 16高地吞 v1.2；1919810/114514；三个版本目录各一份副本",
    ),
    (
        "5400da142acddc2b31dca20c27763542c73d6a9d365bad236fad1da189bfb214",
        "火弦月 双倍速无沟世吞 v3.1；1919810/114514；实算体积反而大于声明值",
    ),
    (
        "8c47a34f611a77020c6b879a681b4747efcfe118beb60e50df75a7942b7eac51",
        "五代世吞 排完海带后单倍速；1919810/114514",
    ),
    (
        "f31930a7169ee66376b427ecc71d0d5f8bf176c38e4a71651f9fb3bcbccc691a",
        "五代世吞 排海带层专用；1919810/114514",
    ),
];

fn known_lie(hash: &str) -> Option<&'static str> {
    KNOWN_META_LIES.iter().find(|(h, _)| *h == hash).map(|(_, why)| *why)
}

fn hash_file(p: &Path) -> String {
    let Ok(bytes) = fs::read(p) else { return String::new() };
    blake3::hash(&bytes).to_hex().to_string()
}

#[derive(Clone)]
enum VerifyOutcome {
    Failed(String),
    NoDeclaration,
    Checked {
        vol_ok: bool,
        blk_ok: bool,
        declared_vol: i64,
        actual_vol: u64,
        declared_blk: i64,
        actual_blk: u64,
        all_consistent: bool,
    },
}

fn verify_one(p: &Path) -> VerifyOutcome {
    let s = match Schematic::load(p, LoadMode::Full) {
        Ok(s) => s,
        Err(e) => return VerifyOutcome::Failed(format!("{e:#}")),
    };
    let (Some(dv), Some(db)) =
        (s.metadata.declared_total_volume, s.metadata.declared_total_blocks)
    else {
        return VerifyOutcome::NoDeclaration;
    };
    let av = s.total_volume();
    let ab = s.total_blocks().unwrap_or(0);
    VerifyOutcome::Checked {
        vol_ok: dv as u64 == av,
        blk_ok: db as u64 == ab,
        declared_vol: dv,
        actual_vol: av,
        declared_blk: db,
        actual_blk: ab,
        all_consistent: s.regions.iter().all(|r| r.is_consistent()),
    }
}

fn cmd_mats(paths: &[PathBuf]) -> Result<()> {
    let opts = MaterialOptions::default();
    let mut merged = MaterialList::default();
    for p in paths {
        let s = Schematic::load(p, LoadMode::Full)?;
        let m = MaterialList::of(&s, &opts);
        println!(
            "+ {}  ({} 个方块 / {} 体积)",
            name(p), m.total_blocks, m.total_volume
        );
        merged.merge(&m);
    }
    println!("\n============== 材料清单{} ==============",
             if paths.len() > 1 { format!("（{} 个蓝图汇总）", paths.len()) } else { String::new() });
    println!("{:<40} {:>12}   {:>6} {:>4} {:>4}", "物品", "总数", "盒", "组", "个");
    println!("{}", "-".repeat(76));
    for (item, n) in merged.sorted_blocks() {
        let ss = stack_size(item);
        let (b, s, i) = to_stacks(n, ss);
        let flag = if merged.inexact.contains(item) { " ~近似" } else { "" };
        println!("{:<40} {:>12}   {:>6} {:>4} {:>4}{}", item, n, b, s, i, flag);
    }
    println!("{}", "-".repeat(76));
    println!("合计 {} 种物品，{} 个方块", merged.blocks.len(), merged.total_blocks);

    if !merged.container_items.is_empty() {
        println!("\n--- 蓝图容器内已有物品（样板物品/备货，不含在上表）---");
        let mut v: Vec<_> = merged.container_items.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (item, n) in v.iter().take(30) {
            println!("  {item:<40} {n:>10}");
        }
        if v.len() > 30 {
            println!("  … 还有 {} 种", v.len() - 30);
        }
    }
    if !merged.inexact.is_empty() {
        println!("\n注意：{} 项使用了近似映射，数字需自行复核：", merged.inexact.len());
        for i in &merged.inexact {
            println!("  {i}");
        }
    }
    Ok(())
}

/// 材质对照表：每个方块一个孤立立方体，按参数顺序排开。
///
/// 渲染出来的机器上光看图猜不出哪块是什么方块，必须有已知答案的对照，
/// 才能判断「方块 ↔ 材质」是不是真的对得上。
fn cmd_sample(colors_path: &Path, out: &Path, blocks: &[&str]) -> Result<()> {
    let (assets, atlas) = load_colors(colors_path)?;
    // 语法：`oak_stairs[facing=east,half=bottom]`，方括号部分可省
    let mut names: Vec<String> = Vec::new();
    let mut props: Vec<BTreeMap<String, String>> = Vec::new();
    for b in blocks {
        let (id, p) = match b.split_once('[') {
            Some((id, rest)) => (id, rest.trim_end_matches(']')),
            None => (*b, ""),
        };
        names.push(if id.contains(':') { id.to_string() } else { format!("minecraft:{id}") });
        props.push(parse_props(p));
    }

    let opts = render::RenderOptions {
        max_px: 190 * names.len().max(1) as u32,
        background: None,
        ..Default::default()
    };
    let grid = render::VoxelGrid::sample_row(&names, &props, &assets, atlas.as_ref(), &opts);
    let img = render::isometric_grid(&grid, atlas.as_ref(), &opts);
    img.save(out)?;

    println!("对照表 → {} ({}×{})", out.display(), img.width(), img.height());
    for (i, n) in names.iter().enumerate() {
        let boxes = assets.resolve(n, &props[i]);
        println!(
            "  #{i} {:<14} {n:<34} {}",
            assets.name_of(n).unwrap_or("-"),
            if boxes.is_empty() {
                "无模型（平均色）".to_string()
            } else {
                format!("{} 个长方体", boxes.len())
            }
        );
        // 逐个列出来。包围盒可能超出 0..16（活塞头的杆、墙上火把），
        // 那不是 bug，标出来免得下次又有人以为要夹回去
        for b in &boxes {
            let out = b.bbox.iter().any(|v| *v < 0 || *v > 16);
            println!(
                "        {:?}{}",
                b.bbox,
                if out { "  ← 伸出格子外（正常）" } else { "" }
            );
        }
    }
    Ok(())
}

/// `facing=east,half=bottom` → 属性表
fn parse_props(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// 表面体素统计。3D 预览就是靠这份数据在前端做 instanced 渲染，
/// 关键指标是「可见体素数」——它直接决定 WebGL 那边的绘制量和 IPC 传输量。
fn cmd_voxels(file: &Path, colors_path: &Path, max_grid: u32) -> Result<()> {
    let (colors, atlas) = load_colors(colors_path)?;
    let t0 = std::time::Instant::now();
    let schem = Schematic::load(file, LoadMode::Full)?;
    let load_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let opts = render::RenderOptions { target_grid: max_grid, ..Default::default() };
    let grid = render::VoxelGrid::build(&schem, &colors, atlas.as_ref(), &opts)
        .ok_or_else(|| anyhow::anyhow!("蓝图没有 region"))?;
    let build_ms = t1.elapsed().as_millis();

    let t2 = std::time::Instant::now();
    let surface = grid.surface_voxels();
    let surf_ms = t2.elapsed().as_millis();

    let solid = grid.solid_count();
    println!("{}", name(file));
    println!(
        "  网格 {}×{}×{} (1体素={}³方块)  调色板 {} 种",
        grid.w, grid.h, grid.d, grid.scale, grid.palette.len()
    );
    println!(
        "  实心体素 {}  →  表面体素 {}  (剔除 {:.1}%)",
        solid,
        surface.len(),
        if solid > 0 { (solid - surface.len()) as f64 * 100.0 / solid as f64 } else { 0.0 }
    );
    println!(
        "  IPC 载荷 {:.2} MB (8 字节/体素，base64 后 {:.2} MB)",
        surface.len() as f64 * 8.0 / 1e6,
        surface.len() as f64 * 8.0 * 4.0 / 3.0 / 1e6
    );

    // 前端是按「长方体」发 instance 的，一个栅栏就是 5 个。体素数只是下限，
    // 这个数才是 WebGL 真正的绘制量——也是 3D 视图会不会卡死的那个数。
    let mut inst = [0usize; 3];
    for (_, _, _, bi) in &surface {
        let Some(b) = grid.palette.get(*bi as usize) else { continue };
        let n = if grid.scale > 1 { 1 } else { b.boxes.len().max(1) };
        inst[match b.opacity {
            render::Opacity::Opaque => 0,
            render::Opacity::Cutout => 1,
            render::Opacity::Translucent => 2,
        }] += n;
    }
    let total_inst = inst[0] + inst[1] + inst[2];
    println!(
        "  instance 数 {total_inst} (不透明 {} / 剪切 {} / 半透明 {})，膨胀 {:.2}×",
        inst[0],
        inst[1],
        inst[2],
        if surface.is_empty() { 0.0 } else { total_inst as f64 / surface.len() as f64 }
    );
    // 半透明方块要单独一趟画，列出来便于核对分类对不对
    let see_through: Vec<&render::VoxelBlock> =
        grid.palette.iter().filter(|b| b.opacity != render::Opacity::Opaque).collect();
    if !see_through.is_empty() {
        println!("  看得穿的方块 {} 种:", see_through.len());
        for b in see_through.iter().take(8) {
            println!("    {:?}  {}", b.opacity, b.name);
        }
    }
    println!("  解析 {load_ms}ms  建网格 {build_ms}ms  表面提取 {surf_ms}ms");

    // 抽查几个体素能不能反查出方块名——悬停提示就靠这个
    println!("  抽样体素:");
    for (x, y, z, bi) in surface.iter().step_by((surface.len() / 5).max(1)).take(5) {
        let b = grid.palette.get(*bi as usize);
        println!(
            "    ({x:>4},{y:>4},{z:>4}) -> {}",
            b.map_or("<越界>", |b| b.name.as_str())
        );
    }
    Ok(())
}

/// 扫描 + 按内容去重。索引模式，不解 BlockStates。
fn cmd_scan(dir: &Path) -> Result<()> {
    let files = collect(dir);
    println!("扫描 {} 个文件（索引模式，跳过 BlockStates）…", files.len());
    let t0 = std::time::Instant::now();

    let rows: Vec<(PathBuf, Result<(u64, usize, u64), String>)> = files
        .par_iter()
        .map(|p| {
            let r = Schematic::load(p, LoadMode::Index)
                .map(|s| {
                    let bbox = s.bounding_box();
                    let dims = bbox.map_or(0, |(lo, hi)| {
                        ((hi.x - lo.x + 1) as u64)
                            * ((hi.y - lo.y + 1) as u64)
                            * ((hi.z - lo.z + 1) as u64)
                    });
                    (s.total_volume(), s.regions.len(), dims)
                })
                .map_err(|e| format!("{e:#}"));
            (p.clone(), r)
        })
        .collect();

    let elapsed = t0.elapsed();
    let ok = rows.iter().filter(|(_, r)| r.is_ok()).count();
    println!(
        "索引完成: {ok}/{} 成功，耗时 {:.2}s ({:.1} ms/file)",
        rows.len(),
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / rows.len().max(1) as f64
    );

    // 按 (体积, region 数, 文件大小) 粗分组，找出跨目录的重复副本
    let mut groups: BTreeMap<(u64, usize, u64), Vec<&PathBuf>> = BTreeMap::new();
    for (p, r) in &rows {
        if let Ok((vol, nreg, _)) = r {
            let size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            groups.entry((*vol, *nreg, size)).or_default().push(p);
        }
    }
    let dupes: usize = groups.values().filter(|v| v.len() > 1).map(|v| v.len() - 1).sum();
    println!(
        "唯一蓝图: {}   冗余副本: {dupes}   可省 {:.1}%",
        groups.len(),
        dupes as f64 * 100.0 / ok.max(1) as f64
    );

    println!("\n副本最多的 10 个:");
    let mut by_count: Vec<_> = groups.iter().filter(|(_, v)| v.len() > 1).collect();
    by_count.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for ((vol, nreg, _), paths) in by_count.iter().take(10) {
        println!("  ×{}  体积={vol} region={nreg}  {}", paths.len(), name(paths[0]));
    }
    Ok(())
}

fn name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |s| s.to_string_lossy().into_owned())
}
