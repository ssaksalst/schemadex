import { invoke } from '@tauri-apps/api/core'
import type {
  AssetsStatus,
  AtlasInfo,
  BlueprintDetail,
  MaterialsResult,
  ScanResult,
  VoxelModel,
} from './types'

export const api = {
  assetsStatus: () => invoke<AssetsStatus>('assets_status'),
  suggestJars: () => invoke<string[]>('suggest_jars'),
  buildAssets: (jar: string) => invoke<AssetsStatus>('build_assets', { jar }),
  scan: (roots: string[]) => invoke<ScanResult>('scan', { roots }),
  thumbnail: (path: string, id: string) => invoke<string>('thumbnail', { path, id }),
  slice: (path: string, y: number, cellPx: number) =>
    invoke<string>('slice', { path, y, cellPx }),
  detail: (path: string) => invoke<BlueprintDetail>('detail', { path }),
  voxels: (path: string, maxGrid: number) => invoke<VoxelModel>('voxels', { path, maxGrid }),
  atlas: () => invoke<AtlasInfo>('atlas'),
  names: () => invoke<Record<string, string>>('names'),
  materials: (paths: string[], countFluids: boolean) =>
    invoke<MaterialsResult>('materials', { paths, countFluids }),
  suggestRoots: () => invoke<string[]>('suggest_roots'),
}

/**
 * 缩略图请求限流。
 *
 * 一屏能出现几十张卡片，全部同时 invoke 会让后端并行解析几十个蓝图——
 * 其中可能有 5 亿方块的巨物，瞬间吃光内存。限制同时在跑的渲染数。
 */
class ThumbQueue {
  private running = 0
  private queue: (() => void)[] = []
  private cache = new Map<string, string>()

  constructor(private readonly limit = 4) {}

  async get(path: string, id: string): Promise<string> {
    const hit = this.cache.get(id)
    if (hit) return hit
    await this.acquire()
    try {
      const url = await api.thumbnail(path, id)
      this.cache.set(id, url)
      return url
    } finally {
      this.release()
    }
  }

  private acquire(): Promise<void> {
    if (this.running < this.limit) {
      this.running++
      return Promise.resolve()
    }
    return new Promise((resolve) => this.queue.push(resolve))
  }

  private release() {
    const next = this.queue.shift()
    if (next) next()
    else this.running--
  }
}

export const thumbs = new ThumbQueue(4)
