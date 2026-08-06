import { useEffect, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import type { AssetsStatus } from '../types'

/**
 * 首次运行的材质表引导页。
 *
 * 方块贴图、模型、中文译名都是 Minecraft 的素材，**不能随程序分发**，
 * 所以不内置、也不下载，而是从用户自己已经装好的客户端 jar 里提取一份，
 * 存进本机的应用数据目录。谁装了游戏谁就有素材，我们既不复制也不传播。
 */
export function AssetsSetup({ onReady }: { onReady: (s: AssetsStatus) => void }) {
  const [jars, setJars] = useState<string[] | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api
      .suggestJars()
      .then(setJars)
      .catch(() => setJars([]))
  }, [])

  const build = async (jar: string) => {
    setBusy(jar)
    setError(null)
    try {
      onReady(await api.buildAssets(jar))
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }

  const pick = async () => {
    const picked = await open({
      multiple: false,
      title: '选择 Minecraft 客户端 jar',
      filters: [{ name: 'Minecraft 客户端', extensions: ['jar'] }],
    })
    if (typeof picked === 'string') await build(picked)
  }

  return (
    <div className="flex h-full items-center justify-center p-8">
      <div className="w-full max-w-2xl">
        <h1 className="text-base font-semibold tracking-tight text-ink-200">
          先生成一次材质表
        </h1>
        <p className="mt-3 text-xs leading-relaxed text-ink-400">
          Schemadex 要用方块贴图、模型和中文译名才能画出蓝图。
          这些是 Minecraft 自己的素材，<strong className="text-ink-300">不随程序分发</strong>
          ，需要从你本机已经装好的客户端里提取一份。只做一次，之后一直用。
        </p>

        {jars === null && <p className="mt-6 text-xs text-ink-500">正在查找客户端…</p>}

        {jars !== null && jars.length > 0 && (
          <>
            <p className="mt-6 text-[11px] uppercase tracking-wide text-ink-500">
              找到这些客户端，挑一个
            </p>
            <ul className="mt-2 space-y-1.5">
              {jars.map((j) => (
                <li key={j}>
                  <button
                    disabled={busy !== null}
                    onClick={() => build(j)}
                    className="group flex w-full items-center gap-3 rounded border border-ink-700 bg-ink-900 px-3 py-2 text-left transition-colors hover:border-ink-500 disabled:opacity-50"
                  >
                    <span className="min-w-0 flex-1 truncate text-xs text-ink-300" title={j}>
                      {j}
                    </span>
                    <span className="shrink-0 text-[11px] text-ink-500 group-hover:text-ink-300">
                      {busy === j ? '提取中…' : '用这个'}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </>
        )}

        {jars !== null && jars.length === 0 && (
          <p className="mt-6 text-xs text-ink-500">
            没自动找到客户端。手动选一个 <code className="text-ink-400">.jar</code>，
            通常在 <code className="text-ink-400">.minecraft/versions/&lt;版本&gt;/</code> 下面。
          </p>
        )}

        <button
          disabled={busy !== null}
          onClick={pick}
          className="mt-4 rounded border border-ink-700 px-3 py-1.5 text-xs text-ink-300 transition-colors hover:border-ink-500 disabled:opacity-50"
        >
          手动选择 jar…
        </button>

        {busy && (
          <p className="mt-4 text-xs text-ink-400">
            正在从 jar 提取贴图和模型，大概十几秒，别关窗口。
          </p>
        )}
        {error && (
          <p className="mt-4 rounded border border-accent/40 bg-accent/10 px-3 py-2 text-xs text-accent">
            {error}
          </p>
        )}
      </div>
    </div>
  )
}
