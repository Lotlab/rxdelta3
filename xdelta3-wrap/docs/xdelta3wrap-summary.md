# XDelta3WrapFactory.dll 逆向摘要

> 供编写替代 DLL 参考。所有地址基于 Ghidra 分析（ImageBase `0x10000000`）。

## 0. 模块概况

- 32 位 x86 PE，ImageBase `0x10000000`，MSVC 编译（含 `__security_check_cookie`、SEH、宽字符）。
- 文件路径全部用 `wchar_t*`（UTF-16）。所有导出函数为 **`__stdcall`**（从 `RET 0x…` 栈清理确认）。
- 4 个合并函数只是“包装器”：在栈上构造一个临时 `CXDelta3WrapImpl` 对象（含临界区），经虚表调用内部实现，再销毁。
- 导出既有命名导出（名字表 `0x102016a9`），Ghidra 另识别出 `CreateExecutor/ReleaseExecutor` 两个序号导出（`0x10008cb0` / `0x10008d40`，与本功能无关，可空实现）。

## 1. 导出表（名字表 + 地址表）

| 导出名 | 地址 | 说明 |
|---|---|---|
| GenerateDeltaDir | 0x10002040 | 生成目录 diff（可空实现）|
| GenerateDeltaDirCustomDiff | 0x100021b0 | 生成自定义 diff（可空实现）|
| GenerateDeltaFile | 0x10001fd0 | 生成单文件 diff（可空实现）|
| GetClassObj | 0x10001e30 | 类工厂（可空实现）|
| **MergeDir** | 0x10001f60 | 目录合并（无自定义 diff）|
| **MergeDirCustomDiff** | 0x100020d0 | 目录合并（自定义 diff）|
| **MergeDirCustomDiffV2** | 0x10002140 | 目录合并（V2，带命令总进度）|
| **MergeFile** | 0x10001ef0 | 单文件合并 |
| ReleaseClassObj | 0x10001ea0 | 类工厂释放（可空实现）|

按约定：除 4 个 Merge 外，`GenerateDeltaDir / GenerateDeltaDirCustomDiff / GenerateDeltaFile / GetClassObj / ReleaseClassObj / CreateExecutor / ReleaseExecutor` 均可返回空实现（如 `return false;` / `return NULL;` / `return 1;`）。

## 2. 四个函数的精确签名（已用反汇编核实）

```c
// 全部 __stdcall，返回 BOOL（true=成功）
BOOL MergeFile(const wchar_t* pSrc, const wchar_t* pDelta, const wchar_t* pDst);

// p4 = 单文件进度回调（与 CustomDiff 的 pFileProgressCb 同一个槽位）
BOOL MergeDir(const wchar_t* pSrcDir, const wchar_t* pPatchDir,
              const wchar_t* pDstDir, void* pFileProgressCb);

BOOL MergeDirCustomDiff(const wchar_t* pSrcDir, const wchar_t* pPatchDir,
                        const wchar_t* pDstDir,
                        void* pFileProgressCb,      // 单文件进度回调
                        void* pMergeCb);            // 自定义合并回调，可为 NULL

BOOL MergeDirCustomDiffV2(const wchar_t* pSrcDir, const wchar_t* pPatchDir,
                          const wchar_t* pDstDir,
                          void* pFileProgressCb,    // 单文件进度回调
                          void* pCmdProgressCb,     // 命令总进度回调(done,total)
                          void* pMergeCb);          // 自定义合并回调，可为 NULL
```

`RET` 栈清理字节数：MergeFile = 0xc（3 参）、MergeDir = 0x10（4 参）、MergeDirCustomDiff = 0x14（5 参）、MergeDirCustomDiffV2 = 0x18（6 参）。

### 回调类型（由调用点反汇编确认，回调为 __stdcall）

```c
// 单文件进度回调：message 是格式化好的宽字符串（含文件路径），type 是消息类别
typedef void (__stdcall *FileProgressCb)(const wchar_t* msg, int type);
//   type: 0 = 进度/开始/完成消息（开始与完成都是 0）
//         1 = 拷贝文件失败
//         2 = 磁盘空间不足 / xdelta 应用失败 / MD5 校验不通过
//         3 = 删除文件失败

// 命令总进度回调（仅 V2 传入；V1 / 其它传 0）
typedef void (__stdcall* CmdProgressCb)(int done, int total);

// 自定义合并回调：代替内部 xdelta 解码（非 NULL 时 MergeFileInternal 直接调用）
// 返回 true = 成功；语义：把 delta 应用到 src，输出到 out
typedef BOOL (__stdcall* MergeCb)(const wchar_t* src, const wchar_t* delta,
                                  const wchar_t* out);
```

## 3. 公共对象 CXDelta3WrapImpl 与虚表

对象大小 0x2C，虚表 `0x101df6c4`：

```
+0x00 vft   = 0x101df6c4
+0x04 flag1 = 1
+0x08 flag2 = 0
+0x0c flag3 = 1
+0x10 CRITICAL_SECTION crit   (InitializeCriticalSectionAndSpinCount(crit, 4000))
```

虚表关键槽位：

| 偏移 | 地址 | 方法 |
|---|---|---|
| +0x0c | … | ReleaseClassObj 调用的释放（`(*(vft+0xc))(1)` 表示删除）|
| +0x24 | 0x10009b00 | MergeDir 的虚拟转发器（再转到 +0x2c）|
| +0x2c | 0x10009610 | MergeDirCustomDiffImpl（三个目录合并的共同入口）|
| +0x34 | 0x1000b7c0 | MergeFileInternal（MergeFile 的实现）|

## 4. 各函数详细逻辑

### 4.1 MergeFile (0x10001ef0) → MergeFileInternal (0x1000b7c0)

包装器：构造临时对象 → 调 `vft[+0x34]`（MergeFileInternal）`(this, pSrc, pDelta, pDst, NULL)` → 销毁对象。

MergeFileInternal 流程：

1. `dstPath` = 拷贝 pDst（SSO 宽字符串）。
2. 若 `__wcsicmp(pSrc, pDst) == 0` → **原地更新**：`dstPath` 追加 `.tmp`，置 `bInPlace = 1`。
3. `required = GetFileSize(pSrc) + GetFileSize(pDelta)`；`CheckDiskSpace(msg, pSrc, &required)` 预检磁盘空间，不足 → 返回 false。
4. 执行合并：
   - `pMergeCb == NULL` → 内部实现 `XDelta3Process(this, &result, pDelta, pSrc, dstPath)`；
   - 否则 → `result = pMergeCb(pSrc, pDelta, dstPath)`。
5. 原地更新收尾：
   - 失败 → `DeleteFileW(dstPath)`（删掉 `.tmp`）；
   - 成功 → `DeleteFileW(pSrc)`，`MoveFileW(dstPath, pSrc)`；若全局标志 `DAT_1020f1f0 != 0`，`MoveFileW` 最多重试 30 次（每次 `Sleep(50)`，失败记 log4cplus 日志）。
6. 返回结果。

### 4.2 内部解码链：XDelta3Process (0x10002540) → XDelta3StreamApply (0x100234a0)

- `XDelta3Process(__thiscall, this, bool* pEncodeMode, wchar_t* pDelta, wchar_t* pSrc, wchar_t* pDst)`：
  1. `EnterCriticalSection(&this->crit)`；
  2. 对 pDelta / pSrc / pDst 依次 `EnsureDirectoryPath`；
  3. `__wfopen_s(fDelta, pDelta, "rb")`、`fSrc "rb"`、`fOut "wb"`；
  4. `XDelta3StreamApply(*pEncodeMode, fDelta, fSrc, fOut)`；
  5. 关闭三个流，返回 `iRet == 0`。
- `XDelta3StreamApply(__fastcall, byte bEncode, FILE* fDelta, FILE* fSrc, FILE* fOut)`：
  流式状态机，`bEncode == 0` 走 apply/decode（VCDIFF 解码），非 0 走 encode。缓冲默认 8MB、按 delta 文件大小自适应、最小 16KB。**合并方向即 `bEncode = 0`。**

> 替换实现可以直接复用 rxdelta 的 apply 逻辑（VCDIFF / RFC3284 + xdelta3 扩展），把文件流换成 mmap / read 即可。

### 4.3 MergeDir (0x10001f60)

包装器 → 调 `vft[+0x24]`（0x10009b00 转发器）`(p1, p2, p3, p4, 0)`；转发器（`RET 0x14`，5 栈参）再通过 `(this, p1, p2, p3, p4, 0, 0)` 虚拟转发到 `vft[+0x2c]`（MergeDirCustomDiffImpl）。

**实际等效**：`MergeDirCustomDiffImpl(this, srcDir, patchDir, dstDir, fileCb = p4, cmdCb = 0, mergeCb = 0)`。

### 4.4 MergeDirCustomDiff (0x100020d0)

直接调 `vtable->+0x2c`：`MergeDirCustomDiffImpl(this, srcDir, patchDir, dstDir, fileCb, cmdCb = 0, mergeCb)`。

### 4.5 MergeDirCustomDiffV2 (0x10002140)

直接调 `vtable->+0x2c`：`MergeDirCustomDiffImpl(this, srcDir, patchDir, dstDir, fileCb, cmdCb, mergeCb)`。

### 4.6 MergeDirCustomDiffImpl (0x10009610) —— 目录合并总调度

```
__thiscall (this, wchar_t* pSrcDir, wchar_t* pPatchDir, wchar_t* pDstDir,
            void* pFileProgressCb, void* pCmdProgressCb, void* pMergeCb)
```

1. `effDst = (pDstDir != NULL) ? pDstDir : pSrcDir`（pDstDir 可传 NULL，等价于原地）。
2. 若 `__wcsicmp(pSrcDir, effDst) != 0`：
   - 目标路径末尾缺 `\` 或 `/` 则补 `\`；`EnsureDirectoryPath(effDst)`。
   - 组装命令行 `xcopy.exe "<pSrcDir>" "<effDst>" /e /y`（宽字符串）。
   - `CreateProcessW(NULL, cmdline, …, STARTF_USESHOWWINDOW | SW_HIDE, &si, &pi)`；
     失败 → 清理并返回 false；成功 → `WaitForSingleObject(pi.hProcess, INFINITE)` + `CloseHandle` × 2。
3. `total = CountPatchTotalCommands(pPatchDir)`（统计 `patch_delta_direct.dat` 四个 XML 节的总命令数）。
4. 依次执行（共用 `curCount` 计数；全部成功才返回 true）：
   ```
   ProcessEmptyPathInfo(...)
   ProcessDeletePathInfo(...)
   ProcessNewPathInfo(...)
   ProcessDeltaPathInfo(...)
   ```

### 4.7 CountPatchTotalCommands (0x10009270)

`__cdecl int CountPatchTotalCommands(wchar_t* pPatchDir)`：拼 `<pPatchDir>\patch_delta_direct.dat`（XML），用 `ParseIniSection`（名称沿 PDB，实为 XML 节解析）依次解析 `EmptyPathInfo / DelPathInfo / NewPathInfo / DeltaPathInfo` 四个节，累加各节节点数返回。

### 4.8 patch_delta_direct.dat 格式（XML 文本）

`patch_delta_direct.dat` 内容为 XML（非 INI）：根元素 `<XMLROOT>`，各节是根下的命名空间元素，每节内是若干 `<KeyPathSubItem>` 条目，每个条目含一对 `<Key>…</Key><Value>…</Value>`（二进制中可确认 `SubItem / Key / Value / XMLROOT / STYLE_XML` 等字符串）。

- `<EmptyPathInfo>`：空文件/直接拷贝的文件列表（`Key/Value` 两个字符串）。
- `<DelPathInfo>`：需要删除的文件列表。
- `<NewPathInfo>`：新增文件（树）列表（两个字符串：源名、目标名）。
- `<DeltaPathInfo>`：`name → patch文件名`（使用 patchDir 里的 `.xdelta` 文件）。
- `<ResultMD5Info>`：`name → md5hex`（目标文件的期望 MD5，用于跳过已补丁文件 + 补丁后校验）。
- 子条目（后缀 `SubItem`）包括：NewPath / NewMD5 / DeltaPath / DeltaMD5 / OriginMD5 / ResultMD5 / EmptyPath / DelPath / DelMD5 / Unknown。

每节由 `ParseIniSection(path, sectionName, list&)`（PDB 名，实为 XML 节解析器）解析进 0x40 字节节点链表；节点内以 SSO 形式存 `Key`（+0x10）与 `Value`（+0x28）两个宽字符串。

### 4.9 四个 Process* 函数（`__thiscall`）

参数统一为：`this, int* pCurCount, int nTotal, pSrcDir, pPatchDir, pDstDir, pFileCb, pCmdCb, pMergeCb`。

- **ProcessEmptyPathInfo (0x10008a80)**：解析 `<EmptyPathInfo> + <ResultMD5Info>`。对每条拼源路径 `srcDir` `\` `name` 与目标路径（输出目录）`…\name` → `EnsureDirectoryPath` → `fileCb(msg, 0)`（"正在拷贝文件：%s"）→ `CheckDiskSpace`（不足→`fileCb(msg, 2)` 并失败）→ 目标已存在则清只读 → `CopyFileW(src, dst, FALSE)`（失败 → `fileCb(msg, 1)`）→ 成功 `*pCurCount++`。
- **ProcessDeletePathInfo (0x10008f40)**：解析 `<DelPathInfo>`。对每条若文件存在：清只读 → `fileCb(msg, 0)` → 删除（`DeleteFileW` 包装）；失败 → `fileCb(msg, 3)` 并中止；成功 `*pCurCount++`、`cmdCb(done, total)`。
- **ProcessNewPathInfo (0x10007bc0)**：解析 `<NewPathInfo>`。对每条调用 `FUN_100078a0`（递归复制目录树）从源复制到目标；成功 `*pCurCount++`、`cmdCb(done, total)`。
- **ProcessDeltaPathInfo (0x100085b0)**：解析 `<DeltaPathInfo> + <ResultMD5Info>`。对每条调用 `ApplySinglePatch(...)`；成功 `*pCurCount++`、`cmdCb(done, total)`。

### 4.10 ApplySinglePatch (0x10007ff0)

```
__thiscall (this, pSrcDir, pPatchDir, pDstDir,
            pFileName, pPatchName, pResultMd5List, pFileProgressCb, unused, pMergeCb)
```

1. 拼路径：`delta = patchDir\patchName`、`base = srcDir\name`、`out = dstDir\name`。
2. `fileCb(msg, 0)`（开始消息）。
3. 若 `pResultMd5List` 中有 `name` 且已存在文件 `GetFileMd5Hex(base) == 期望md5` → **直接跳过**（已是最新）。
4. `CheckDiskSpace(msg, name, GetFileSize(base) + GetFileSize(delta))`，不足 → `fileCb(msg, 2)`、失败。
5. base 已存在则清只读。
6. 调 `vtable->+0x34`（MergeFileInternal）`(base, delta, out, pMergeCb)`。
7. 成功：`fileCb(msg, 0)`；若该文件在 md5 列表 → `GetFileMd5Hex(out)` 与期望比对，不符 → `fileCb(msg, 2)` 并判失败。失败：`fileCb(msg, 2)`。

## 5. 替换 DLL 建议

- 导出 4 个合并函数（同名、`__stdcall`）；其余导出空实现。
- 回调用 `__stdcall`，参数形状严格按第 2 节。
- 关键语义：
  1. `pDstDir` 为 NULL 表示原地（等价于 `pSrcDir`）；
  2. 源 ≠ 目标时先 `xcopy.exe Src Dst /e /y` 复制整树；
  3. 进度：`fileCb(msg, type)` 每个文件一次、`cmdCb(done, total)` 每条命令一次，`total` = 4 节命令数之和；
  4. `<ResultMD5Info>` 同时用于“跳过已补丁文件”和“补丁后校验”；
  5. 单文件应用可复用 rxdelta（等价 `bEncode = 0` 的 VCDIFF 解码）。
- 每类失败要通过 `fileCb(msg, 1 / 2 / 3)` 上报并中断，返回 false。