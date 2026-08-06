import { useEffect, useRef, useState } from 'react'
import * as THREE from 'three'
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js'
import { api } from '../api'
import type { AtlasInfo, ResolvedBox, VoxelModel } from '../types'
import { formatCount, itemLabel } from '../types'

/** base64 → Uint8Array（后端按 8 字节/体素 打包：小端 u16 的 x,y,z,调色板索引） */
function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/** 这面要画但没贴图 → 用平均色。渲染器自造的占位长方体也用它 */
const NO_TILE = 65535
/** 模型压根没声明这一面 → 不画。和后端 mcassets::NO_FACE 一致 */
const NO_FACE = 65534
void NO_FACE // 只在着色器里按数值用到，这里留个名字免得下次有人再猜 65534 是什么
// 与后端 mcassets::model 的面下标一致：up, down, north, south, east, west

/**
 * 把一个面的 uv 矩形与旋转压进一个 float。
 *
 * 六个面各要 4 个 uv 分量加 1 个旋转，摊开就是 30 个 instanced 属性，
 * 顶点属性槽不够用。uv 分量都是 0..16 的整数、旋转是 0..3，
 * 用 17 进制打包成一个数，最大 334083，float32 的 24 位尾数能精确表示。
 */
function packUv(rect: [number, number, number, number], rot: number): number {
  return rect[0] + rect[1] * 17 + rect[2] * 289 + rect[3] * 4913 + (rot & 3) * 83521
}

interface Hover {
  name: string
  /** 中文名，没有译名时为 null */
  label: string | null
  x: number
  y: number
  /** 该方块在蓝图局部坐标系里的位置 */
  pos: [number, number, number]
}

/** 图集只跟版本有关，整个会话取一次就够，别每开一个蓝图就传一遍几百 KB */
let atlasPromise: Promise<AtlasInfo> | null = null
function getAtlas(): Promise<AtlasInfo> {
  if (!atlasPromise) atlasPromise = api.atlas()
  return atlasPromise
}

export function Viewer3D({ path, size }: { path: string; size: [number, number, number] }) {
  const mountRef = useRef<HTMLDivElement>(null)
  /** 鼠标在容器内的像素坐标，渲染循环里读它来定位提示框 */
  const hoverScreen = useRef({ x: 0, y: 0 })
  const [model, setModel] = useState<VoxelModel | null>(null)
  const [atlas, setAtlas] = useState<AtlasInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [hover, setHover] = useState<Hover | null>(null)
  const [loading, setLoading] = useState(true)

  // 大蓝图降采样后一个体素代表多个方块，网格分辨率按体量给，别让小蓝图也被降精度
  const maxGrid = Math.max(size[0], size[1], size[2]) <= 192 ? 256 : 176

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    Promise.all([api.voxels(path, maxGrid), getAtlas()])
      .then(([m, a]) => {
        if (cancelled) return
        setModel(m)
        setAtlas(a)
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false))
    return () => {
      cancelled = true
    }
  }, [path, maxGrid])

  useEffect(() => {
    const mount = mountRef.current
    if (!mount || !model || !atlas) return

    const scene = new THREE.Scene()
    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true })
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2))
    renderer.domElement.style.display = 'block'
    mount.appendChild(renderer.domElement)

    const camera = new THREE.PerspectiveCamera(45, 1, 0.1, 20000)
    const controls = new OrbitControls(camera, renderer.domElement)
    controls.enableDamping = true
    controls.dampingFactor = 0.08

    const [dx, dy, dz] = model.dims
    const center = new THREE.Vector3(dx / 2, dy / 2, dz / 2)
    controls.target.copy(center)

    // 默认视角对齐缩略图的等距方位，从库视图点进来不会有方向错乱感
    const radius = Math.max(dx, dy, dz) * 1.6 + 8
    camera.position.set(center.x + radius, center.y + radius * 0.8, center.z + radius)
    camera.lookAt(center)
    controls.update()

    // 半球光给底色，方向光拉出方块的朝向差异；纯环境光会让整个模型糊成一片
    scene.add(new THREE.HemisphereLight(0xffffff, 0x40404a, 2.0))
    const dir = new THREE.DirectionalLight(0xffffff, 1.0)
    dir.position.set(1, 1.4, 0.7)
    scene.add(dir)
    const fill = new THREE.DirectionalLight(0xffffff, 0.3)
    fill.position.set(-1, 0.3, -0.8)
    scene.add(fill)

    // 材质图集。像素画放大绝不能插值，必须最近邻且关掉 mipmap，
    // 否则缩小时相邻图块会互相渗色。
    const texLoader = new THREE.TextureLoader()
    const atlasTex = texLoader.load(atlas.image)
    atlasTex.magFilter = THREE.NearestFilter
    atlasTex.minFilter = THREE.NearestFilter
    atlasTex.generateMipmaps = false
    atlasTex.colorSpace = THREE.SRGBColorSpace
    atlasTex.wrapS = THREE.ClampToEdgeWrapping
    atlasTex.wrapT = THREE.ClampToEdgeWrapping

    const bytes = decodeBase64(model.data)
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    const voxelCount = model.count

    // 一个方块可能由多个长方体组成（楼梯两段、栅栏柱子加横杆），
    // 每个长方体是一个 instance。先数一遍总数才能开缓冲。
    const FULL_UV: [number, number, number, number] = [0, 0, 16, 16]
    const FULL_BOX: ResolvedBox = {
      bbox: [0, 0, 0, 16, 16, 16],
      faces: [NO_TILE, NO_TILE, NO_TILE, NO_TILE, NO_TILE, NO_TILE],
      uv: [FULL_UV, FULL_UV, FULL_UV, FULL_UV, FULL_UV, FULL_UV],
      rot: [0, 0, 0, 0, 0, 0],
    }
    // 降采样时一个体素代表多个方块，此时统一按完整立方体画。
    // **六面要贴这个方块的代表图块**——退回平均色的话，
    // 只要蓝图最长边超过后端的 target_grid，整个模型就全是色块。
    const reprCache = new Map<number, ResolvedBox[]>()
    const reprBoxOf = (bi: number): ResolvedBox[] => {
      let v = reprCache.get(bi)
      if (!v) {
        const r = model.palette[bi]?.repr
        const [top, side] = r ?? [NO_TILE, NO_TILE]
        v = [{ ...FULL_BOX, faces: [top, top, side, side, side, side] }]
        reprCache.set(bi, v)
      }
      return v
    }
    const boxesOf = (bi: number): ResolvedBox[] => {
      if (model.scale > 1) return reprBoxOf(bi)
      const b = model.palette[bi]?.boxes
      return b && b.length > 0 ? b : reprBoxOf(bi)
    }

    /**
     * 染色玻璃必须单独一趟画。
     *
     * alphaTest 只能做「留或不留」的二选一，而染色玻璃**每一个纹素**的 alpha
     * 都在 0.4~0.64 之间——没有一个会被剪掉，材质又不是 transparent，
     * 于是 alpha 被整个丢弃，染色玻璃就渲染成一块实心色板。
     * 这类方块只能开真正的 alpha 混合，也就必须和不透明的分成两个 mesh。
     */
    const isTranslucent = (bi: number) => model.palette[bi]?.opacity === 'translucent'

    // 先按组各数一遍，才开得出对的缓冲大小
    let countOpaque = 0
    let countGlass = 0
    for (let i = 0; i < voxelCount; i++) {
      const bi = view.getUint16(i * 8 + 6, true)
      const n = boxesOf(bi).length
      if (isTranslucent(bi)) countGlass += n
      else countOpaque += n
    }

    const patchShader = (shader: THREE.WebGLProgramParametersWithUniforms) => {
      shader.uniforms.uTilesPerRow = { value: atlas.tiles_per_row }
      shader.vertexShader = shader.vertexShader
        .replace(
          '#include <common>',
          `#include <common>
           attribute vec3 aTilesA;     // 图块索引 [上, 下, 北]
           attribute vec3 aTilesB;     // 图块索引 [南, 东, 西]
           attribute vec3 aUvA;        // 打包的 uv+旋转 [上, 下, 北]
           attribute vec3 aUvB;        // 打包的 uv+旋转 [南, 东, 西]
           attribute vec3 aFallback;   // 无材质时的平均色
           varying float vTile;
           varying float vPacked;
           varying vec2 vTileUv;
           varying vec3 vFallback;`,
        )
        .replace(
          '#include <begin_vertex>',
          `#include <begin_vertex>
           // BoxGeometry 的法线是轴对齐的，六个面各取各的贴图。
           // MC 的北是 -Z、南是 +Z、东是 +X、西是 -X。
           if (normal.y > 0.5)       { vTile = aTilesA.x; vPacked = aUvA.x; }
           else if (normal.y < -0.5) { vTile = aTilesA.y; vPacked = aUvA.y; }
           else if (normal.z < -0.5) { vTile = aTilesA.z; vPacked = aUvA.z; }
           else if (normal.z > 0.5)  { vTile = aTilesB.x; vPacked = aUvB.x; }
           else if (normal.x > 0.5)  { vTile = aTilesB.y; vPacked = aUvB.y; }
           else                      { vTile = aTilesB.z; vPacked = aUvB.z; }
           vTileUv = uv;
           vFallback = aFallback;`,
        )
      shader.fragmentShader = shader.fragmentShader
        .replace(
          '#include <common>',
          `#include <common>
           uniform float uTilesPerRow;
           varying float vTile;
           varying float vPacked;
           varying vec2 vTileUv;
           varying vec3 vFallback;`,
        )
        .replace(
          '#include <map_fragment>',
          `// varying 插值后 756.0 可能变成 755.9999，而 756/28 恰好是整数边界，
           // 直接 floor 会掉一整行图块。先取整再算行列。
           float tile = floor(vTile + 0.5);
           if (tile > 65534.5) {
             // NO_TILE：这面要画但没贴图，用平均色
             diffuseColor.rgb *= vFallback;
           } else if (tile > 65533.5) {
             // NO_FACE：模型没声明这一面，MC 不画，我们也不画。
             // 置零让不透明那趟的 alphaTest 剪掉、半透明那趟混成零贡献。
             diffuseColor.a = 0.0;
           } else {
             // 解包 uv 矩形与旋转（17 进制，见前端的 packUv）
             float packed = floor(vPacked + 0.5);
             float u1 = mod(packed, 17.0);
             float v1 = mod(floor(packed / 17.0), 17.0);
             float u2 = mod(floor(packed / 289.0), 17.0);
             float v2 = mod(floor(packed / 4913.0), 17.0);
             float rot = floor(packed / 83521.0);

             vec2 p = clamp(vTileUv, 0.002, 0.998);
             // 面内旋转：红石线的走向就靠它
             if (rot > 2.5)      p = vec2(1.0 - p.y, p.x);
             else if (rot > 1.5) p = vec2(1.0 - p.x, 1.0 - p.y);
             else if (rot > 0.5) p = vec2(p.y, 1.0 - p.x);

             // uv 是「自上而下」的贴图坐标，而 BoxGeometry 的 uv.y 自下而上
             vec2 tex = vec2(u1 + p.x * (u2 - u1), v1 + (1.0 - p.y) * (v2 - v1)) / 16.0;

             float row = floor(tile / uTilesPerRow + 0.0001);
             float col = tile - row * uTilesPerRow;
             // 纵向必须翻转：图集的 0 号图块在图片左上角，行号自上而下数；
             // 而纹理坐标 v=0 在底部（three 默认 flipY，v=1 才是图片顶部）。
             float u = (col + clamp(tex.x, 0.002, 0.998)) / uTilesPerRow;
             float v = 1.0 - (row + clamp(tex.y, 0.002, 0.998)) / uTilesPerRow;
             vec4 texel = texture2D(map, vec2(u, v));
             diffuseColor *= texel;
           }`,
        )
    }

    /**
     * 一趟绘制所需的全套东西。两组的着色器完全相同，只有混合方式不同。
     *
     * 每个 instance 六个面各自的图块与 uv。InstancedMesh 共用同一份几何体，
     * 贴图选择只能靠 instanced 属性 + 改 shader。
     *
     * 六个面必须分开传：只给「顶/侧/底」三个槽的话四个侧面会共用一张贴图，
     * 朝北的粘性活塞就会四面都是黏液绿。
     */
    const makeGroup = (count: number, translucent: boolean) => {
      const geom = new THREE.BoxGeometry(1, 1, 1)
      const mat = new THREE.MeshLambertMaterial({
        map: atlasTex,
        ...(translucent
          ? {
              // 染色玻璃：真做 alpha 混合。
              // 必须关掉深度写入——否则先画到的那片玻璃会把它背后的东西
              // 挡在深度测试外面，机器又变成看不见的了。
              // instanced 没法逐片排序，玻璃叠玻璃的地方会偏暗，
              // 但那恰好也是真玻璃叠起来的样子，可以接受。
              transparent: true,
              depthWrite: false,
            }
          : {
              // 树叶、原版玻璃的贴图是「全透或全不透」，剪切掉就行，不需要排序
              alphaTest: 0.35,
            }),
      })
      mat.onBeforeCompile = patchShader
      // onBeforeCompile 改了源码，得让 three 知道这是个不同的 program
      mat.customProgramCacheKey = () => `atlas-${atlas.tiles_per_row}`

      const mesh = new THREE.InstancedMesh(geom, mat, count)
      mesh.instanceMatrix.setUsage(THREE.StaticDrawUsage)
      // 半透明的必须后画，否则它背后的不透明方块还没画上去
      mesh.renderOrder = translucent ? 1 : 0

      return {
        geom,
        mat,
        mesh,
        tilesA: new Float32Array(count * 3), // up, down, north
        tilesB: new Float32Array(count * 3), // south, east, west
        uvA: new Float32Array(count * 3),
        uvB: new Float32Array(count * 3),
        // 没有材质的方块退回平均色，由这个属性传给 shader
        fallback: new Float32Array(count * 3),
        // 保留每个 instance 的坐标与方块索引，raycast 命中后要用它反查
        blockIndex: new Uint16Array(count),
        coords: new Uint16Array(count * 3),
        n: 0,
      }
    }
    type Group = ReturnType<typeof makeGroup>

    const opaqueGroup = makeGroup(countOpaque, false)
    const glassGroup = makeGroup(countGlass, true)
    const groups: Group[] = [opaqueGroup, glassGroup]

    const dummy = new THREE.Object3D()

    for (let i = 0; i < voxelCount; i++) {
      const o = i * 8
      const x = view.getUint16(o, true)
      const y = view.getUint16(o + 2, true)
      const z = view.getUint16(o + 4, true)
      const bi = view.getUint16(o + 6, true)
      const entry = model.palette[bi]
      const g = isTranslucent(bi) ? glassGroup : opaqueGroup

      const rgb = entry ? entry.top : [158, 158, 158]
      // 平均色是 sRGB，直接塞进线性工作流会发灰
      const c = new THREE.Color(rgb[0] / 255, rgb[1] / 255, rgb[2] / 255).convertSRGBToLinear()

      for (const bx of boxesOf(bi)) {
        const n = g.n
        const b = bx.bbox
        dummy.scale.set(
          Math.max((b[3] - b[0]) / 16, 0.02),
          Math.max((b[4] - b[1]) / 16, 0.02),
          Math.max((b[5] - b[2]) / 16, 0.02),
        )
        dummy.position.set(x + (b[0] + b[3]) / 32, y + (b[1] + b[4]) / 32, z + (b[2] + b[5]) / 32)
        dummy.updateMatrix()
        g.mesh.setMatrixAt(n, dummy.matrix)

        // 六个面各自的图块与 uv，顺序与后端一致：up, down, north, south, east, west
        for (let f = 0; f < 3; f++) {
          g.tilesA[n * 3 + f] = bx.faces[f]
          g.tilesB[n * 3 + f] = bx.faces[f + 3]
          g.uvA[n * 3 + f] = packUv(bx.uv[f], bx.rot[f])
          g.uvB[n * 3 + f] = packUv(bx.uv[f + 3], bx.rot[f + 3])
        }

        g.fallback[n * 3] = c.r
        g.fallback[n * 3 + 1] = c.g
        g.fallback[n * 3 + 2] = c.b

        g.blockIndex[n] = bi
        g.coords[n * 3] = x
        g.coords[n * 3 + 1] = y
        g.coords[n * 3 + 2] = z
        g.n++
      }
    }

    for (const g of groups) {
      g.mesh.instanceMatrix.needsUpdate = true
      g.geom.setAttribute('aTilesA', new THREE.InstancedBufferAttribute(g.tilesA, 3))
      g.geom.setAttribute('aTilesB', new THREE.InstancedBufferAttribute(g.tilesB, 3))
      g.geom.setAttribute('aUvA', new THREE.InstancedBufferAttribute(g.uvA, 3))
      g.geom.setAttribute('aUvB', new THREE.InstancedBufferAttribute(g.uvB, 3))
      g.geom.setAttribute('aFallback', new THREE.InstancedBufferAttribute(g.fallback, 3))
      if (g.mesh.count > 0) scene.add(g.mesh)
    }

    // 悬停高亮：一个略大的线框跟着走，比改 instance 颜色简单且不用重传缓冲。
    // 用单位立方体做基准，命中时按方块实际占位缩放。
    const highlight = new THREE.LineSegments(
      new THREE.EdgesGeometry(new THREE.BoxGeometry(1, 1, 1)),
      new THREE.LineBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.9 }),
    )
    highlight.visible = false
    scene.add(highlight)

    const raycaster = new THREE.Raycaster()
    const pointer = new THREE.Vector2()
    let pointerInside = false

    const resize = () => {
      const w = mount.clientWidth
      const h = mount.clientHeight
      if (w === 0 || h === 0) return
      // 第三个参数必须留默认的 true：设成 false 只改绘图缓冲不改 CSS 尺寸，
      // canvas 会按 devicePixelRatio 撑大到容器外，模型就跑到右下角去了
      renderer.setSize(w, h)
      camera.aspect = w / h
      camera.updateProjectionMatrix()
    }
    resize()
    const ro = new ResizeObserver(resize)
    ro.observe(mount)

    const onPointerMove = (e: PointerEvent) => {
      const r = renderer.domElement.getBoundingClientRect()
      pointer.x = ((e.clientX - r.left) / r.width) * 2 - 1
      pointer.y = -((e.clientY - r.top) / r.height) * 2 + 1
      pointerInside = true
      hoverScreen.current = { x: e.clientX - r.left, y: e.clientY - r.top }
    }
    const onPointerLeave = () => {
      pointerInside = false
      setHover(null)
      highlight.visible = false
    }
    renderer.domElement.addEventListener('pointermove', onPointerMove)
    renderer.domElement.addEventListener('pointerleave', onPointerLeave)

    let raf = 0
    let lastPick = 0
    const animate = (t: number) => {
      raf = requestAnimationFrame(animate)
      controls.update()

      // raycast 不必每帧做，30ms 一次足够跟手，还省 CPU
      if (pointerInside && t - lastPick > 30) {
        lastPick = t
        raycaster.setFromCamera(pointer, camera)
        // 两组都要打，取最近的那个命中——玻璃也该能被悬停查到
        const hits = raycaster.intersectObjects(
          groups.filter((g) => g.mesh.count > 0).map((g) => g.mesh),
          false,
        )
        const hit = hits[0]
        const id = hit?.instanceId
        const g = groups.find((q) => q.mesh === hit?.object)
        if (id !== undefined && g) {
          const bi = g.blockIndex[id]
          const entry = model.palette[bi]
          const gx = g.coords[id * 3]
          const gy = g.coords[id * 3 + 1]
          const gz = g.coords[id * 3 + 2]
          // 高亮整格而非单个长方体——楼梯、栅栏由多块拼成，
          // 只框住命中的那一块反而看不清是哪个方块
          highlight.scale.set(1.04, 1.04, 1.04)
          highlight.position.set(gx + 0.5, gy + 0.5, gz + 0.5)
          highlight.visible = true
          setHover({
            name: entry?.name ?? 'unknown',
            label: entry?.label ?? null,
            x: hoverScreen.current.x,
            y: hoverScreen.current.y,
            pos: [gx * model.scale, gy * model.scale, gz * model.scale],
          })
        } else {
          highlight.visible = false
          setHover(null)
        }
      }
      renderer.render(scene, camera)
    }
    raf = requestAnimationFrame(animate)

    return () => {
      cancelAnimationFrame(raf)
      ro.disconnect()
      renderer.domElement.removeEventListener('pointermove', onPointerMove)
      renderer.domElement.removeEventListener('pointerleave', onPointerLeave)
      controls.dispose()
      for (const g of groups) {
        g.geom.dispose()
        g.mat.dispose()
        g.mesh.dispose()
      }
      atlasTex.dispose()
      highlight.geometry.dispose()
      ;(highlight.material as THREE.Material).dispose()
      renderer.dispose()
      if (renderer.domElement.parentNode === mount) mount.removeChild(renderer.domElement)
    }
  }, [model, atlas])

  return (
    <div className="relative h-full w-full overflow-hidden">
      <div ref={mountRef} className="h-full w-full cursor-grab active:cursor-grabbing" />

      {loading && (
        <div className="absolute inset-0 flex items-center justify-center text-sm text-ink-500">
          构建体素模型…
        </div>
      )}
      {error && (
        <div className="absolute inset-0 flex items-center justify-center px-6 text-center text-sm text-accent">
          {error}
        </div>
      )}

      {hover && (
        <div
          className="pointer-events-none absolute z-10 max-w-xs rounded border border-ink-600 bg-ink-950/95 px-2 py-1 shadow-lg"
          style={{
            left: Math.min(hover.x + 14, (mountRef.current?.clientWidth ?? 0) - 210),
            top: hover.y + 14,
          }}
        >
          <div className="text-xs text-ink-100">{hover.label ?? itemLabel(hover.name)}</div>
          <div className="tnum text-[11px] text-ink-500">
            {hover.name} · ({hover.pos[0]}, {hover.pos[1]}, {hover.pos[2]})
          </div>
        </div>
      )}

      {model && (
        <div className="pointer-events-none absolute bottom-2 left-3 text-[11px] leading-relaxed text-ink-500">
          <div>
            {formatCount(model.count)} 个可见体素 · {model.palette.length} 种方块
            {model.scale > 1 && (
              <span className="text-amber-500/80"> · 1 体素 = {model.scale}³ 方块</span>
            )}
          </div>
          <div>左键拖拽旋转 · 滚轮缩放 · 右键平移 · 悬停查看方块</div>
        </div>
      )}
    </div>
  )
}
