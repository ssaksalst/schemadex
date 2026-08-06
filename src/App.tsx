import { useEffect, useMemo, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { api } from './api'
import { Thumb } from './components/Thumb'
import { DetailModal } from './components/DetailModal'
import { MaterialsPanel } from './components/MaterialsPanel'
import { AssetsSetup } from './components/AssetsSetup'
import type { AssetsStatus, Blueprint, ScanResult } from './types'
import { formatBytes, formatCount } from './types'

type SortKey = 'name' | 'size' | 'modified'

const ROOTS_KEY = 'litevault.roots'

export default function App() {
  const [roots, setRoots] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem(ROOTS_KEY) ?? '[]')
    } catch {
      return []
    }
  })
  const [scanResult, setScanResult] = useState<ScanResult | null>(null)
  const [scanning, setScanning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [sort, setSort] = useState<SortKey>('name')
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [detailOf, setDetailOf] = useState<Blueprint | null>(null)
  // 材质表不随程序分发，首次运行要从用户自己的客户端 jar 生成。
  // null = 还没查出来，挡一帧空白比闪一下引导页好
  const [assets, setAssets] = useState<AssetsStatus | null>(null)

  useEffect(() => {
    api.assetsStatus().then(setAssets).catch(() => setAssets({ ready: false, version: null, dir: null }))
  }, [])

  useEffect(() => {
    localStorage.setItem(ROOTS_KEY, JSON.stringify(roots))
  }, [roots])

  // 首次进来没有配置目录时，主动去猜——国内玩家多用 PCL/HMCL，
  // 蓝图不在官方启动器那个默认目录里
  useEffect(() => {
    if (roots.length > 0) return
    api.suggestRoots().then((found) => {
      if (found.length > 0) setRoots(found)
    })
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  const addRoot = async () => {
    const picked = await open({ directory: true, multiple: true, title: '选择蓝图目录' })
    if (!picked) return
    const list = Array.isArray(picked) ? picked : [picked]
    setRoots((prev) => Array.from(new Set([...prev, ...list])))
  }

  const doScan = async () => {
    if (roots.length === 0) return
    setScanning(true)
    setError(null)
    try {
      setScanResult(await api.scan(roots))
      setSelected(new Set())
    } catch (e) {
      setError(String(e))
    } finally {
      setScanning(false)
    }
  }

  const blueprints = scanResult?.blueprints ?? []

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase()
    const list = q
      ? blueprints.filter(
          (b) =>
            b.file_name.toLowerCase().includes(q) ||
            (b.name?.toLowerCase().includes(q) ?? false) ||
            (b.author?.toLowerCase().includes(q) ?? false),
        )
      : blueprints
    const sorted = [...list]
    if (sort === 'name') sorted.sort((a, b) => a.file_name.localeCompare(b.file_name, 'zh-CN'))
    if (sort === 'size') sorted.sort((a, b) => b.volume - a.volume)
    if (sort === 'modified') sorted.sort((a, b) => (b.modified ?? 0) - (a.modified ?? 0))
    return sorted
  }, [blueprints, query, sort])

  const selectedList = useMemo(
    () => blueprints.filter((b) => selected.has(b.id)),
    [blueprints, selected],
  )

  const toggle = (bp: Blueprint) =>
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(bp.id)) next.delete(bp.id)
      else next.add(bp.id)
      return next
    })

  if (assets === null) return <div className="h-full" />
  if (!assets.ready) return <AssetsSetup onReady={setAssets} />

  return (
    <div className="flex h-full flex-col">
      {/* ---------- 顶栏 ---------- */}
      <header className="flex shrink-0 items-center gap-3 border-b border-ink-800 bg-ink-900 px-4 py-2.5">
        <h1 className="shrink-0 text-sm font-semibold tracking-tight text-ink-200">LiteVault</h1>
        {/* 材质表是从哪个版本的客户端提取的。选错了 jar 得有路可退，
            否则只能自己去删应用数据目录 */}
        <button
          onClick={() => setAssets({ ...assets, ready: false })}
          title={`材质表来自 ${assets.version ?? '未知版本'}\n${assets.dir ?? ''}\n点击改用别的客户端重新生成`}
          className="shrink-0 rounded border border-ink-800 px-1.5 py-0.5 text-[10px] text-ink-500 transition-colors hover:border-ink-600 hover:text-ink-300"
        >
          {assets.version ?? '材质表'}
        </button>

        <div className="flex min-w-0 flex-1 items-center gap-2">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索蓝图名 / 作者…"
            className="w-64 rounded border border-ink-700 bg-ink-950 px-2.5 py-1 text-xs text-ink-200 placeholder:text-ink-500 focus:border-ink-500 focus:outline-none"
          />
          <select
            value={sort}
            onChange={(e) => setSort(e.target.value as SortKey)}
            className="rounded border border-ink-700 bg-ink-950 px-2 py-1 text-xs text-ink-300 focus:outline-none"
          >
            <option value="name">按名称</option>
            <option value="size">按体积</option>
            <option value="modified">按修改时间</option>
          </select>
        </div>

        {scanResult && (
          <span className="tnum shrink-0 text-xs text-ink-400">
            {formatCount(scanResult.unique)} 个蓝图
            {scanResult.total_files > scanResult.unique && (
              <span className="text-ink-500">
                {' '}
                / {formatCount(scanResult.total_files)} 文件（去重
                {Math.round(
                  ((scanResult.total_files - scanResult.unique) / scanResult.total_files) * 100,
                )}
                %）
              </span>
            )}
          </span>
        )}

        <button
          onClick={addRoot}
          className="shrink-0 rounded border border-ink-700 px-2.5 py-1 text-xs text-ink-300 hover:bg-ink-800"
        >
          添加目录
        </button>
        <button
          onClick={doScan}
          disabled={scanning || roots.length === 0}
          className="shrink-0 rounded bg-accent px-3 py-1 text-xs font-medium text-white hover:brightness-110 disabled:opacity-40"
        >
          {scanning ? '扫描中…' : '扫描'}
        </button>
      </header>

      {/* ---------- 目录条 ---------- */}
      {roots.length > 0 && (
        <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-ink-800 bg-ink-900/60 px-4 py-1.5">
          {roots.map((r) => (
            <span
              key={r}
              className="group flex max-w-md items-center gap-1 rounded bg-ink-800 px-2 py-0.5 text-[11px] text-ink-400"
              title={r}
            >
              <span className="truncate">{r}</span>
              <button
                onClick={() => setRoots((p) => p.filter((x) => x !== r))}
                className="shrink-0 text-ink-500 opacity-0 transition-opacity group-hover:opacity-100 hover:text-accent"
              >
                ✕
              </button>
            </span>
          ))}
        </div>
      )}

      {error && (
        <div className="shrink-0 border-b border-accent/40 bg-accent/10 px-4 py-2 text-xs text-accent">
          {error}
        </div>
      )}

      {/* ---------- 主体 ---------- */}
      <div className="flex min-h-0 flex-1">
        <main className="scroll-thin min-w-0 flex-1 overflow-y-auto p-4">
          {blueprints.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 text-center text-sm text-ink-500">
              {roots.length === 0 ? (
                <>
                  <p>还没有配置蓝图目录</p>
                  <p className="text-xs">
                    通常在 <code className="text-ink-400">.minecraft/schematics</code>
                    ；用 PCL / HMCL 且开了版本隔离的话，
                    <br />
                    每个 <code className="text-ink-400">versions/&lt;版本&gt;/schematics</code>{' '}
                    下都有一份
                  </p>
                </>
              ) : (
                <p>点「扫描」开始</p>
              )}
            </div>
          ) : (
            <>
              <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-3">
                {visible.map((bp) => {
                  const on = selected.has(bp.id)
                  return (
                    <div
                      key={bp.id}
                      onClick={() => toggle(bp)}
                      onDoubleClick={() => setDetailOf(bp)}
                      className={`group relative cursor-pointer overflow-hidden rounded-lg border transition-colors ${
                        on
                          ? 'border-accent bg-accent/10'
                          : 'border-ink-800 bg-ink-900 hover:border-ink-600'
                      }`}
                    >
                      {/* 双击能开详情，但没人猜得到。悬停给个明确入口。 */}
                      <button
                        onClick={(e) => {
                          e.stopPropagation()
                          setDetailOf(bp)
                        }}
                        title="3D 预览 / 逐层切片"
                        className="absolute right-1.5 top-1.5 z-10 rounded bg-ink-950/80 px-1.5 py-0.5 text-[11px] text-ink-300 opacity-0 transition-opacity hover:bg-ink-800 hover:text-ink-100 group-hover:opacity-100"
                      >
                        打开
                      </button>
                      <Thumb path={bp.path} id={bp.id} />
                      <div className="p-2">
                        <p
                          className="truncate text-xs text-ink-200"
                          title={bp.name ?? bp.file_name}
                        >
                          {bp.name ?? bp.file_name.replace(/\.litematic$/i, '')}
                        </p>
                        <p className="tnum mt-0.5 flex items-center gap-1.5 text-[11px] text-ink-500">
                          <span>
                            {bp.size[0]}×{bp.size[1]}×{bp.size[2]}
                          </span>
                          <span className="text-ink-700">·</span>
                          <span>{formatBytes(bp.file_size)}</span>
                          {bp.duplicates.length > 0 && (
                            <span
                              className="rounded bg-ink-800 px-1 text-ink-400"
                              title={`还有 ${bp.duplicates.length} 份相同副本：\n${bp.duplicates.join('\n')}`}
                            >
                              ×{bp.duplicates.length + 1}
                            </span>
                          )}
                        </p>
                      </div>
                    </div>
                  )
                })}
              </div>
              {visible.length === 0 && (
                <p className="mt-8 text-center text-sm text-ink-500">没有匹配的蓝图</p>
              )}
              <p className="mt-4 text-center text-[11px] text-ink-600">
                单击选中 · 双击打开 3D 预览与逐层切片
              </p>
            </>
          )}
        </main>

        <aside className="w-96 shrink-0 border-l border-ink-800 bg-ink-900">
          <MaterialsPanel selected={selectedList} />
        </aside>
      </div>

      {/* ---------- 底栏 ---------- */}
      <footer className="flex shrink-0 items-center justify-between border-t border-ink-800 bg-ink-900 px-4 py-1.5 text-[11px] text-ink-500">
        <span>
          {selected.size > 0 ? (
            <>
              已选 <span className="tnum text-ink-300">{selected.size}</span> 个
              <button
                onClick={() => setSelected(new Set())}
                className="ml-2 text-ink-400 hover:text-accent"
              >
                清空
              </button>
              <button
                onClick={() => setSelected(new Set(visible.map((b) => b.id)))}
                className="ml-2 text-ink-400 hover:text-ink-200"
              >
                全选当前视图
              </button>
            </>
          ) : (
            '未选中蓝图'
          )}
        </span>
        {scanResult && (
          <span className="tnum">
            扫描耗时 {scanResult.elapsed_ms} ms
            {scanResult.failed.length > 0 && (
              <span className="ml-2 text-amber-500">{scanResult.failed.length} 个解析失败</span>
            )}
          </span>
        )}
      </footer>

      {detailOf && <DetailModal bp={detailOf} onClose={() => setDetailOf(null)} />}
    </div>
  )
}
