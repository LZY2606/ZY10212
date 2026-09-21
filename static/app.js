"use strict";

const state = {
  data: null,
  selected: 0,
  surfacePickMode: false,
  checked: new Set(),
};

const $ = (sel) => document.querySelector(sel);

async function api(method, url, body) {
  const opts = { method, headers: {} };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = typeof body === "string" ? body : JSON.stringify(body);
  }
  const res = await fetch(url, opts);
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = await res.json();
      if (j.error) msg = j.error;
    } catch (_) {}
    throw new Error(msg);
  }
  const ct = res.headers.get("content-type") || "";
  return ct.includes("application/json") ? res.json() : res.text();
}

function toast(msg, isErr) {
  const t = $("#toast");
  t.textContent = msg;
  t.className = isErr ? "toast err" : "toast";
  clearTimeout(toast._h);
  toast._h = setTimeout(() => t.classList.add("hidden"), 2600);
}

function pos() {
  return state.data.positions[state.selected];
}

function fmt(x, d = 3) {
  if (x === null || x === undefined || Number.isNaN(x)) return "—";
  return Number(x).toFixed(d);
}

async function refresh() {
  state.data = await api("GET", "/api/state");
  if (!state.data.fixture) {
    renderEmpty();
    return;
  }
  if (state.selected >= state.data.positions.length) state.selected = 0;
  renderConfig();
  renderGrid();
  renderGates();
  renderPosition();
  renderCandidates();
  renderTracks();
}

function renderEmpty() {
  $("#grid").innerHTML = "";
  $("#gates").innerHTML = "";
  $("#tracks").innerHTML = "";
  $("#pos-title").textContent = "数据库为空";
  const ctx = $("#wave-canvas").getContext("2d");
  ctx.clearRect(0, 0, 1200, 420);
  ctx.fillStyle = "#93a1bc";
  ctx.font = "16px sans-serif";
  ctx.fillText("数据库中没有夹具数据，请点击右上角“清空并复位”导入内置固定夹具。", 40, 80);
  document.querySelector("#candidates tbody").innerHTML = "";
}

function renderConfig() {
  const c = state.data.config;
  $("#cfg-velocity").value = Math.round(c.velocity_m_s);
  $("#cfg-delay").value = Math.round(c.probe_delay_ns);
  $("#cfg-mode").value = c.threshold_mode;
  $("#cfg-amp").value = c.amp_threshold;
  $("#cfg-snr").value = c.snr_threshold;
  $("#cfg-res").value = Math.round(c.min_resolvable_ns);
}

function renderGrid() {
  const fx = state.data.fixture;
  const cols = fx.grid.cols;
  const grid = $("#grid");
  grid.style.gridTemplateColumns = `repeat(${cols}, 1fr)`;
  grid.innerHTML = "";
  state.data.positions.forEach((p, i) => {
    const cell = document.createElement("div");
    cell.className = "cell" + (i === state.selected ? " active" : "");
    cell.title = `${p.name}  x=${p.x_mm}mm y=${p.y_mm}mm`;
    cell.textContent = p.name;
    const cands = p.analysis.candidates;
    if (cands.some((c) => c.saturated)) addDot(cell, "sat");
    else if (cands.some((c) => c.composite && !c.manual_split)) addDot(cell, "comp");
    else if (cands.some((c) => c.verdict && c.verdict !== "rejected")) addDot(cell, "");
    cell.addEventListener("click", () => {
      state.selected = i;
      state.surfacePickMode = false;
      refresh();
    });
    grid.appendChild(cell);
  });
}

function addDot(cell, cls) {
  const d = document.createElement("span");
  d.className = "dot " + cls;
  cell.appendChild(d);
}

function renderGates() {
  const wrap = $("#gates");
  wrap.innerHTML = "";
  state.data.gates.forEach((g) => {
    const row = document.createElement("div");
    row.className = "gate-row";
    row.innerHTML =
      `<span title="${g.role}">${g.label}</span>` +
      `<input type="number" value="${g.start}" min="0" data-f="start" />` +
      `<input type="number" value="${g.end}" min="0" data-f="end" />` +
      `<button>保存</button>`;
    row.querySelector("button").addEventListener("click", async () => {
      const start = Number(row.querySelector('[data-f=start]').value);
      const end = Number(row.querySelector('[data-f=end]').value);
      try {
        await api("PUT", `/api/gates/${encodeURIComponent(g.id)}`, { start, end });
        toast("闸门已更新（左闭右开）");
        await refresh();
      } catch (e) { toast(e.message, true); }
    });
    wrap.appendChild(row);
  });
}

function renderPosition() {
  const p = pos();
  const a = p.analysis;
  const inverted = a.error === "surface_after_bottom";
  $("#pos-title").textContent =
    `位置 ${p.name}（索引 ${p.index}，x=${p.x_mm}mm，y=${p.y_mm}mm）`;
  const banner = $("#position-banner");
  if (inverted) {
    banner.className = "banner";
    banner.textContent =
      "表面到达晚于底面候选：禁止倒置深度轴，本位置不输出深度结果。请修正表面到达时刻。";
  } else {
    banner.className = "banner hidden";
    banner.textContent = "";
  }
  $("#surface-mode-hint").textContent = state.surfacePickMode
    ? "（点击波形设置表面到达样本）"
    : "";
  drawWave(p, inverted);
}

function drawWave(p, inverted) {
  const cv = $("#wave-canvas");
  const ctx = cv.getContext("2d");
  const W = cv.width, H = cv.height;
  ctx.clearRect(0, 0, W, H);
  const ml = 56, mr = inverted ? 18 : 64, mt = 14, mb = 42;
  const pw = W - ml - mr, ph = H - mt - mb;
  const n = p.wave.length;
  const x = (i) => ml + (i / (n - 1)) * pw;
  const maxEnv = Math.max(0.2, ...p.envelope);
  const y = (v) => mt + (1 - (v + 1) / 2) * ph; // raw wave in [-1,1]
  const ye = (v) => mt + ph - (v / maxEnv) * ph;

  // gate bands (left-closed/right-open visual hint)
  state.data.gates.forEach((g) => {
    const gx0 = x(g.start), gx1 = x(g.end);
    ctx.fillStyle = "rgba(79,156,255,0.07)";
    ctx.fillRect(gx0, mt, gx1 - gx0, ph);
    ctx.strokeStyle = "rgba(79,156,255,0.55)";
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(gx0, mt); ctx.lineTo(gx0, mt + ph);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = "#7fa8e0";
    ctx.font = "10px sans-serif";
    ctx.fillText(g.label, gx0 + 3, mt + 11);
  });

  // zero line
  ctx.strokeStyle = "#33415c";
  ctx.beginPath(); ctx.moveTo(ml, y(0)); ctx.lineTo(ml + pw, y(0)); ctx.stroke();

  // saturation full-scale rails
  ctx.strokeStyle = "rgba(239,91,91,0.25)";
  [1, -1].forEach((v) => {
    ctx.beginPath(); ctx.moveTo(ml, y(v)); ctx.lineTo(ml + pw, y(v)); ctx.stroke();
  });

  // raw waveform
  ctx.strokeStyle = "#7ea4d8";
  ctx.lineWidth = 1;
  ctx.beginPath();
  p.wave.forEach((v, i) => {
    const xx = x(i), yy = y(v);
    i ? ctx.lineTo(xx, yy) : ctx.moveTo(xx, yy);
  });
  ctx.stroke();

  // envelope
  ctx.strokeStyle = "#ffd166";
  ctx.lineWidth = 1.4;
  ctx.beginPath();
  p.envelope.forEach((v, i) => {
    const xx = x(i), yy = ye(v);
    i ? ctx.lineTo(xx, yy) : ctx.moveTo(xx, yy);
  });
  ctx.stroke();

  // threshold (envelope scale)
  const thr = p.analysis.threshold_value;
  ctx.strokeStyle = "#ef5b5b";
  ctx.setLineDash([7, 4]);
  ctx.beginPath(); ctx.moveTo(ml, ye(thr)); ctx.lineTo(ml + pw, ye(thr)); ctx.stroke();
  ctx.setLineDash([]);
  ctx.fillStyle = "#ef5b5b";
  ctx.font = "10px sans-serif";
  ctx.fillText(
    `阈值 ${p.analysis.threshold_mode === "snr"
      ? "SNR " + fmt(thr / Math.max(p.analysis.noise_rms, 1e-9), 1)
      : fmt(thr, 2)} (噪声RMS ${fmt(p.analysis.noise_rms, 4)})`,
    ml + 6, ye(thr) - 4);

  // saturated samples overlay
  ctx.fillStyle = "rgba(239,91,91,0.35)";
  p.wave.forEach((v, i) => {
    if (Math.abs(v) >= 1 - 1e-9) ctx.fillRect(x(i) - 0.6, mt, 1.6, ph);
  });

  // surface arrival
  const sx = x(p.surface_sample);
  ctx.strokeStyle = "#37c07a";
  ctx.lineWidth = 2;
  ctx.beginPath(); ctx.moveTo(sx, mt); ctx.lineTo(sx, mt + ph); ctx.stroke();
  ctx.fillStyle = "#37c07a";
  ctx.fillText(`表面 ${p.surface_sample}`, sx + 4, mt + ph - 6);

  // candidate markers
  p.analysis.candidates.forEach((c) => {
    const cx = x(c.peak_sample);
    let col = "#4f9cff";
    if (c.saturated) col = "#ef5b5b";
    else if (c.composite && !c.manual_split) col = "#c084fc";
    else if (c.manual_split) col = "#4f9cff";
    ctx.fillStyle = col;
    ctx.beginPath();
    ctx.arc(cx, ye(c.amp), c.manual_split ? 3 : 4, 0, Math.PI * 2);
    ctx.fill();
    // composite bracket over member peaks
    if (c.composite && !c.manual_split && c.members.length > 1) {
      const ms = c.members.map((m) => m.peak_sample);
      const lo = Math.min(...ms), hi = Math.max(...ms);
      ctx.strokeStyle = "#c084fc";
      ctx.beginPath();
      ctx.moveTo(x(lo), mt + 16); ctx.lineTo(x(hi), mt + 16);
      ctx.moveTo(x(lo), mt + 12); ctx.lineTo(x(lo), mt + 20);
      ctx.moveTo(x(hi), mt + 12); ctx.lineTo(x(hi), mt + 20);
      ctx.stroke();
    }
  });

  drawAxes(ctx, ml, mt, pw, ph, n, p, inverted);
}

function drawAxes(ctx, ml, mt, pw, ph, n, p, inverted) {
  ctx.strokeStyle = "#46566f";
  ctx.fillStyle = "#93a1bc";
  ctx.font = "10px sans-serif";
  ctx.strokeRect(ml, mt, pw, ph);
  const dt = state.data.fixture.sample_interval_ns;
  for (let i = 0; i <= n - 1; i += 100) {
    const xx = ml + (i / (n - 1)) * pw;
    ctx.strokeStyle = "#26324a";
    ctx.beginPath(); ctx.moveTo(xx, mt + ph); ctx.lineTo(xx, mt + ph + 4); ctx.stroke();
    ctx.fillStyle = "#93a1bc";
    ctx.fillText(`${i}`, xx - 8, mt + ph + 15);
    ctx.fillText(`${Math.round(i * dt)}ns`, xx - 12, mt + ph + 28);
  }
  ctx.fillText("样本 / 时间", ml + pw / 2 - 20, mt + ph + 40);
  for (let v = -1; v <= 1.0001; v += 0.5) {
    const yy = mt + (1 - (v + 1) / 2) * ph;
    ctx.fillText(v.toFixed(1), ml - 26, yy + 3);
  }

  if (!inverted) {
    // right depth axis: 4 mm ticks, surface-relative
    const cfg = state.data.config;
    const rangeOf = (depth) => {
      const surfRange =
        ((p.surface_sample * dt - cfg.probe_delay_ns) * 1e-9 * cfg.velocity_m_s) / 2 * 1000;
      return surfRange + depth;
    };
    const sampleOfDepth = (depth) => {
      const r = rangeOf(depth);
      return (2 * r / 1000 / cfg.velocity_m_s / 1e-9 + cfg.probe_delay_ns) / dt;
    };
    const axisX = ml + pw + 6;
    ctx.fillStyle = "#37c07a";
    for (let d = 0; d <= 24; d += 4) {
      const s = sampleOfDepth(d);
      if (s < 0 || s >= n) continue;
      const xx = ml + (s / (n - 1)) * pw;
      ctx.strokeStyle = "rgba(55,192,122,0.5)";
      ctx.beginPath(); ctx.moveTo(xx, mt); ctx.lineTo(xx, mt + ph); ctx.stroke();
      ctx.fillText(`${d}`, axisX + 2, mt + ph - ((s / (n - 1)) * ph) + 3);
    }
    ctx.save();
    ctx.translate(axisX + 48, mt + ph / 2);
    ctx.rotate(-Math.PI / 2);
    ctx.fillText("表面相对深度 (mm)", -40, 0);
    ctx.restore();
  }
}

function renderCandidates() {
  const p = pos();
  const tbody = document.querySelector("#candidates tbody");
  tbody.innerHTML = "";
  p.analysis.candidates.forEach((c) => {
    const tr = document.createElement("tr");
    const tags = [];
    if (c.saturated) tags.push('<span class="tag sat">饱和:幅值≥</span>');
    if (c.composite && !c.manual_split) tags.push('<span class="tag comp">复合</span>');
    if (c.manual_split) tags.push('<span class="tag split">拆分</span>');
    if (c.orphaned) tags.push('<span class="tag orphan">历史结论</span>');
    const verdicts = [
      ["retained", "保留候选"],
      ["true_scatterer", "真实散射"],
      ["saturation", "设备饱和"],
      ["overlap", "重叠回波"],
      ["rejected", "排除"],
    ];
    const options = verdicts
      .map(([v, label]) =>
        `<option value="${v}" ${c.verdict === v ? "selected" : ""}>${label}</option>`)
      .join("");
    const ampText = c.saturated
      ? `≥ ${fmt(c.amp, 3)}`
      : fmt(c.amp, 3);
    tr.innerHTML =
      `<td title="样本区间 [${c.start_sample}, ${c.end_sample})` +
        (c.snapshot_velocity_m_s
          ? `\n结论时刻版本: 声速 ${c.snapshot_velocity_m_s} m/s, 延迟 ${c.snapshot_probe_delay_ns} ns`
          : "") +
        `">${c.key}</td>` +
      `<td>${c.gate_id || "—"}</td>` +
      `<td>${c.peak_sample}</td>` +
      `<td>${fmt(c.time_ns, 1)}</td>` +
      `<td>${fmt(c.range_mm, 2)}</td>` +
      `<td>${fmt(c.depth_mm, 2)}</td>` +
      `<td>${ampText}</td>` +
      `<td>${fmt(c.snr, 1)}</td>` +
      `<td>${tags.join(" ")}</td>` +
      `<td><input type="checkbox" data-act="track"
          data-key="${c.key}" ${state.checked.has(c.key) ? "checked" : ""}/></td>` +
      `<td><select data-act="verdict" class="verdict-${c.verdict || ""}">
          <option value="">(未裁决)</option>${options}</select></td>` +
      `<td class="note-cell"><input type="text" data-act="note"
          value="${(c.note || "").replace(/"/g, "&quot;")}" placeholder="备注"/></td>`;
    tr.querySelector('[data-act=track]').addEventListener("change", (e) => {
      if (e.target.checked) state.checked.add(c.key);
      else state.checked.delete(c.key);
    });
    tr.querySelector('[data-act=verdict]').addEventListener("change", async (e) => {
      const verdict = e.target.value;
      if (!verdict) return;
      const note = tr.querySelector('[data-act=note]').value;
      try {
        await api(
          "PUT",
          `/api/positions/${p.index}/candidates/${encodeURIComponent(c.key)}/decision`,
          { verdict, note }
        );
        toast(`已记录裁决：${verdict}`);
        await refresh();
      } catch (err) { toast(err.message, true); }
    });
    tr.querySelector('[data-act=note]').addEventListener("change", async (e) => {
      if (!c.verdict) { toast("请先选择裁决再保存备注", true); return; }
      try {
        await api(
          "PUT",
          `/api/positions/${p.index}/candidates/${encodeURIComponent(c.key)}/decision`,
          { verdict: c.verdict, note: e.target.value }
        );
        toast("备注已保存");
        await refresh();
      } catch (err) { toast(err.message, true); }
    });
    if (c.composite && !c.manual_split) {
      const td = tr.children[8];
      const btn = document.createElement("button");
      btn.textContent = "拆分";
      btn.style.marginLeft = "5px";
      btn.addEventListener("click", () => splitCandidate(c));
      td.appendChild(btn);
    }
    tbody.appendChild(tr);
  });
}

async function splitCandidate(c) {
  const p = pos();
  const input = prompt(
    `在哪些样本边界拆分复合候选 ${c.key}？\n区间 [${c.start_sample}, ${c.end_sample})，` +
    `边界左闭右开，逗号分隔（留空白取消）。`,
    String(Math.round((c.start_sample + c.end_sample) / 2))
  );
  if (input === null) return;
  const cuts = input
    .split(/[,，\s]+/)
    .map((s) => Number(s.trim()))
    .filter((n) => Number.isInteger(n) && n > c.start_sample && n < c.end_sample);
  if (!cuts.length) return;
  try {
    await api(
      "POST",
      `/api/positions/${p.index}/candidates/${encodeURIComponent(c.key)}/split`,
      { cuts }
    );
    toast("宽回波已拆分为子候选");
    await refresh();
  } catch (e) { toast(e.message, true); }
}

function renderTracks() {
  const wrap = $("#tracks");
  wrap.innerHTML = "";
  state.data.tracks.forEach((t) => {
    const item = document.createElement("div");
    item.className = "track-item";
    const label = t.members
      .map((m) => {
        const pp = state.data.positions[m.position_index];
        return pp ? pp.name : pp === undefined ? `#${m.position_index}` : pp.name;
      })
      .join(" → ");
    item.innerHTML =
      `<strong>#${t.id} ${t.name}</strong>
       <div class="members">${label}（${t.members.length} 个候选）</div>`;
    const del = document.createElement("button");
    del.textContent = "删除轨迹";
    del.addEventListener("click", async () => {
      try {
        await api("DELETE", `/api/tracks/${t.id}`);
        toast("轨迹已删除");
        await refresh();
      } catch (e) { toast(e.message, true); }
    });
    item.appendChild(del);
    wrap.appendChild(item);
  });

  const create = document.createElement("button");
  create.textContent = "用勾选候选新建轨迹";
  create.className = "primary";
  create.style.width = "100%";
  create.addEventListener("click", createTrackFromChecked);
  wrap.appendChild(create);
}

function orderedMembers() {
  // Gather checked candidates across all positions, then order them so
  // consecutive members sit on 4-connected adjacent scan positions.
  const found = [];
  state.data.positions.forEach((pp) => {
    pp.analysis.candidates.forEach((c) => {
      if (state.checked.has(c.key)) {
        found.push({ position_index: pp.index, candidate_key: c.key });
      }
    });
  });
  if (found.length <= 1) return found;
  const byPos = new Map();
  found.forEach((m) => {
    if (!byPos.has(m.position_index)) byPos.set(m.position_index, []);
    byPos.get(m.position_index).push(m);
  });
  const cols = state.data.fixture.grid.cols;
  const start = found[0].position_index;
  const ordered = [byPos.get(start)[0]];
  const used = new Set([`${start}:${ordered[0].candidate_key}`]);
  let cur = start;
  while (ordered.length < found.length) {
    const neighbors = [cur + 1, cur - 1, cur + cols, cur - cols];
    let next = null;
    for (const np of neighbors) {
      const arr = byPos.get(np);
      if (!arr) continue;
      const cand = arr.find((m) => !used.has(`${np}:${m.candidate_key}`));
      if (cand) { next = cand; break; }
    }
    if (!next) break;
    used.add(`${next.position_index}:${next.candidate_key}`);
    ordered.push(next);
    cur = next.position_index;
  }
  return ordered;
}

async function createTrackFromChecked() {
  const members = orderedMembers();
  if (!members.length) {
    toast("请先在候选表勾选要成轨的候选（可跨相邻位置）", true);
    return;
  }
  const name = prompt("轨迹名称", "缺陷轨迹") || "缺陷轨迹";
  try {
    await api("POST", "/api/tracks", { name, members });
    toast(`已用 ${members.length} 个相邻候选建立轨迹`);
    state.checked.clear();
    await refresh();
  } catch (e) { toast(e.message, true); }
}

async function applyConfig() {
  const body = {
    velocity_m_s: Number($("#cfg-velocity").value),
    probe_delay_ns: Number($("#cfg-delay").value),
    threshold_mode: $("#cfg-mode").value,
    amp_threshold: Number($("#cfg-amp").value),
    snr_threshold: Number($("#cfg-snr").value),
    min_resolvable_ns: Number($("#cfg-res").value),
  };
  try {
    await api("PUT", "/api/config", body);
    toast("参数已更新（深度换算使用新声速/延迟版本）");
    await refresh();
  } catch (e) { toast(e.message, true); }
}

async function setSurfaceSample(sample) {
  const p = pos();
  try {
    await api("PUT", `/api/positions/${p.index}/surface`, { surface_sample: sample });
    toast(`表面到达已修正为样本 ${sample}`);
    await refresh();
  } catch (e) { toast(e.message, true); }
}

function downloadJson(name, obj) {
  const blob = new Blob([JSON.stringify(obj, null, 2)], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  a.click();
  URL.revokeObjectURL(a.href);
}

function wire() {
  $("#btn-apply-config").addEventListener("click", applyConfig);

  $("#btn-surface-mode").addEventListener("click", () => {
    state.surfacePickMode = !state.surfacePickMode;
    $("#surface-mode-hint").textContent = state.surfacePickMode
      ? "（点击波形设置表面到达样本）" : "";
  });

  $("#wave-canvas").addEventListener("click", (e) => {
    if (!state.surfacePickMode || !state.data.fixture) return;
    const cv = e.currentTarget;
    const rect = cv.getBoundingClientRect();
    const W = cv.width;
    const ml = 56, mr = 64;
    const pw = W - ml - mr;
    const px = ((e.clientX - rect.left) / rect.width) * W;
    const n = pos().wave.length;
    const sample = Math.round(((px - ml) / pw) * (n - 1));
    if (sample >= 0 && sample < n) {
      state.surfacePickMode = false;
      setSurfaceSample(sample);
    }
  });

  $("#btn-export").addEventListener("click", async () => {
    try {
      const log = await api("GET", "/api/events");
      downloadJson("echo-bench-run.json", { schema: "echo-bench-run/1", events: log.events });
      toast("运行记录已导出");
    } catch (e) { toast(e.message, true); }
  });

  $("#btn-replay").addEventListener("click", () => $("#file-replay").click());
  $("#file-replay").addEventListener("change", async (e) => {
    const f = e.target.files[0];
    if (!f) return;
    try {
      const text = await f.text();
      const parsed = JSON.parse(text);
      const events = parsed.events || parsed;
      const r = await api("POST", "/api/replay", { events });
      toast(`已重放 ${r.replayed} 条事件并复核`);
      await refresh();
    } catch (err) { toast(err.message, true); }
    e.target.value = "";
  });

  $("#btn-reimport").addEventListener("click", () => $("#file-import").click());
  $("#file-import").addEventListener("change", async (e) => {
    const f = e.target.files[0];
    if (!f) return;
    try {
      const text = await f.text();
      await api("POST", "/api/import-fixture", text);
      toast("夹具已重新导入，结论库已清空待复核");
      await refresh();
    } catch (err) { toast(err.message, true); }
    e.target.value = "";
  });

  $("#btn-reset").addEventListener("click", async () => {
    if (!confirm("清空数据库并重新导入内置固定夹具？")) return;
    try {
      await api("POST", "/api/reset", {});
      state.checked.clear();
      toast("数据库已清空并重新导入夹具");
      await refresh();
    } catch (err) { toast(err.message, true); }
  });
}

wire();
refresh().catch((e) => toast(e.message, true));
