export interface Blueprint {
  id: string
  path: string
  duplicates: string[]
  file_name: string
  name: string | null
  author: string | null
  size: [number, number, number]
  volume: number
  region_count: number
  data_version: number
  file_size: number
  modified: number | null
  metadata_trustworthy: boolean
}

export interface FailedFile {
  path: string
  error: string
}

export interface ScanResult {
  blueprints: Blueprint[]
  total_files: number
  unique: number
  failed: FailedFile[]
  elapsed_ms: number
}

export interface MaterialRow {
  item: string
  count: number
  boxes: number
  stacks: number
  rest: number
  stack_size: number
  inexact: boolean
}

export interface MaterialsResult {
  rows: MaterialRow[]
  container_items: MaterialRow[]
  total_blocks: number
  total_volume: number
  blueprint_count: number
  failed: FailedFile[]
}

/** 一个长方体：包围盒（1/16 单位）+ 六面图集索引（up,down,north,south,east,west） */
export interface ResolvedBox {
  bbox: [number, number, number, number, number, number]
  faces: [number, number, number, number, number, number]
  /** 各面的贴图取样矩形 [u1,v1,u2,v2]，单位 1/16 */
  uv: [number, number, number, number][]
  /** 各面的纹理旋转（0~3，单位 90 度） */
  rot: number[]
}

/**
 * 贴图透明度，后端按图集 alpha 判定。
 *
 * - `opaque`：每个纹素都不透明
 * - `cutout`：有全透明纹素、其余全不透（树叶、铁栏杆、原版玻璃）—— alphaTest 剪掉即可
 * - `translucent`：存在半透明纹素（染色玻璃）—— **只能真做 alpha 混合，alphaTest 对它无效**
 */
export type Opacity = 'opaque' | 'cutout' | 'translucent'

export interface VoxelPaletteEntry {
  name: string
  top: [number, number, number]
  side: [number, number, number]
  /** 中文名，没有译名时为 null */
  label: string | null
  /** 按真实方块状态解析出的长方体；空表示没有模型，画一个平均色立方体 */
  boxes: ResolvedBox[]
  opacity: Opacity
  /** 降采样时的代表图块 [顶面, 侧面]，scale > 1 时贴满整格 */
  repr: [number, number]
}

/**
 * 材质表状态。
 *
 * 材质表不随程序分发（那是 Mojang 的素材），首次运行要从用户自己的
 * Minecraft 客户端 jar 生成。`ready === false` 时前端挡在引导页。
 */
export interface AssetsStatus {
  ready: boolean
  /** 生成时用的 MC 版本 */
  version: string | null
  /** 存放位置，显示出来便于排查 */
  dir: string | null
}

export interface AtlasInfo {
  tile_size: number
  tiles_per_row: number
  /** data: URL 形式的 PNG */
  image: string
}

export interface VoxelModel {
  dims: [number, number, number]
  /** 一个体素代表原蓝图多少个方块（每轴）。>1 表示做了降采样 */
  scale: number
  palette: VoxelPaletteEntry[]
  /** base64；解开后每个体素 8 字节：小端 u16 的 x, y, z, 调色板索引 */
  data: string
  count: number
  reduced: boolean
}

export interface BlueprintDetail {
  layers: number
  layer_counts: number[]
  palette_size: number
  top_blocks: [string, number][]
}

/** 中文名优先，没有译名时退回去掉命名空间的英文 ID */
export { label as itemLabel } from './names'

export function formatCount(n: number): string {
  return n.toLocaleString('en-US')
}

/** "3盒 12组 5个" —— 生电备货就是按这个单位说话的 */
export function formatStacks(r: MaterialRow): string {
  if (r.stack_size === 1) return `${formatCount(r.count)} 个`
  const parts: string[] = []
  if (r.boxes > 0) parts.push(`${r.boxes} 盒`)
  if (r.stacks > 0) parts.push(`${r.stacks} 组`)
  if (r.rest > 0 || parts.length === 0) parts.push(`${r.rest} 个`)
  return parts.join(' ')
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}
