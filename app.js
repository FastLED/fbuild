// fbuild lookup site — sql.js front-end.
//
// Loads the daily SQLite database announced by manifest.json, runs a small
// fixed set of parameterized queries against it, and renders results as a
// table. There is no free-form SQL input — the canned queries below are the
// entire surface area.
//
// See https://github.com/FastLED/fbuild/issues/718 for design rationale.

(() => {
  "use strict";

  const $status  = document.getElementById("status");
  const $results = document.getElementById("results");
  const $dbMeta  = document.getElementById("db-meta");

  function setStatus(msg, isError = false) {
    $status.textContent = msg;
    $status.className = "status" + (isError ? " error" : "");
  }

  function renderTable(columns, rows, emptyHint) {
    if (!rows || rows.length === 0) {
      $results.innerHTML = `<p class="hint">${emptyHint || "no rows"}</p>`;
      return;
    }
    const head = `<tr>${columns.map(c => `<th>${escapeHtml(c)}</th>`).join("")}</tr>`;
    const body = rows.map(r =>
      `<tr>${r.map(v => `<td>${escapeHtml(formatCell(v))}</td>`).join("")}</tr>`
    ).join("");
    $results.innerHTML = `<table><thead>${head}</thead><tbody>${body}</tbody></table>`;
  }

  function formatCell(v) {
    if (v === null || v === undefined) return "";
    if (typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= 0xffff) {
      // Likely a VID or PID — render as both decimal and hex for clarity.
      return `0x${v.toString(16).padStart(4, "0")}`;
    }
    if (typeof v === "number" && !Number.isInteger(v)) {
      return v.toFixed(3);
    }
    return String(v);
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  function parseHex4(s) {
    const t = String(s || "").trim().toLowerCase().replace(/^0x/, "");
    if (!/^[0-9a-f]{1,4}$/.test(t)) {
      throw new Error(`invalid 4-hex-digit value: ${JSON.stringify(s)}`);
    }
    return parseInt(t, 16);
  }

  // FTS5 default tokenizer treats `-` as a column reference, so any user
  // query that contains hyphens (`ESP32-S3`) must be quoted before binding.
  function quoteForFts5(s) {
    const safe = String(s).replace(/"/g, "");
    return `"${safe}"`;
  }

  // --------------------------------------------------------------------- //
  // Canned queries — match the SQL committed in test_build_sqlite.py.
  // --------------------------------------------------------------------- //

  const QUERIES = {
    vidpidToBoards: `
      SELECT
        b.id            AS board_id,
        b.name          AS board_name,
        b.vendor        AS board_vendor,
        b.mcu           AS mcu,
        v.vendor        AS usb_vendor,
        p.product       AS usb_product,
        (
          m.score
          + CASE WHEN p.pid IS NOT NULL THEN 0.25 ELSE 0.0 END
          + CASE WHEN LOWER(b.vendor) = LOWER(v.vendor) THEN 0.10 ELSE 0.0 END
        )               AS score
      FROM mcu_to_vid m
      JOIN usb_vendor v
        ON v.vid = m.vid
      LEFT JOIN usb_product p
        ON p.vid = m.vid AND p.pid = ?2
      JOIN board b
        ON b.mcu = m.mcu_family OR b.mcu LIKE m.mcu_family || '%'
      WHERE m.vid = ?1
      ORDER BY score DESC
      LIMIT 10;
    `,
    boardNameToVid: `
      SELECT board_id, board_name, vid, usb_vendor, confidence, reason
      FROM board_vid_guess
      WHERE board_id IN (SELECT id FROM board_fts WHERE board_fts MATCH ?)
      ORDER BY confidence DESC
      LIMIT 20;
    `,
    listByMcu: `
      SELECT id, name, vendor, platform
      FROM board
      WHERE mcu = ? COLLATE NOCASE
      ORDER BY vendor, name
      LIMIT 200;
    `,
    productsUnderVid: `
      SELECT printf('0x%04x', pid) AS pid, product
      FROM usb_product
      WHERE vid = ?
      ORDER BY pid
      LIMIT 200;
    `,
    vendorSearch: `
      SELECT printf('0x%04x', vid) AS vid, vendor
      FROM usb_vendor
      WHERE vendor LIKE ?
      ORDER BY vendor
      LIMIT 100;
    `,
  };

  // --------------------------------------------------------------------- //
  // DB loader: discover manifest.json, fetch current_db, open in sql.js.
  // --------------------------------------------------------------------- //

  let db = null;

  async function loadDatabase() {
    setStatus("loading manifest…");
    const manifestResp = await fetch("manifest.json", { cache: "no-cache" });
    if (!manifestResp.ok) throw new Error(`manifest.json: ${manifestResp.status}`);
    const manifest = await manifestResp.json();
    if (!manifest.current_db) throw new Error("manifest.json has no current_db");
    setStatus(`loading ${manifest.current_db}…`);

    const dbResp = await fetch(manifest.current_db, { cache: "force-cache" });
    if (!dbResp.ok) throw new Error(`${manifest.current_db}: ${dbResp.status}`);
    const bytes = new Uint8Array(await dbResp.arrayBuffer());

    setStatus("initializing sql.js…");
    const SQL = await initSqlJs({
      locateFile: file => file,  // sql-wasm.wasm sits next to sql-wasm.js
    });
    db = new SQL.Database(bytes);

    const sizeKb = (bytes.length / 1024).toFixed(0);
    $dbMeta.textContent =
      `· db: ${manifest.current_db} (${sizeKb} KB, ${manifest.generated_at || "unknown"})`;
    setStatus("ready. pick a query above.");
  }

  function runQuery(sql, params) {
    if (!db) throw new Error("database not loaded yet");
    const stmt = db.prepare(sql);
    try {
      stmt.bind(params);
      const cols = stmt.getColumnNames();
      const rows = [];
      while (stmt.step()) rows.push(stmt.get());
      return { cols, rows };
    } finally {
      stmt.free();
    }
  }

  // --------------------------------------------------------------------- //
  // Form wiring
  // --------------------------------------------------------------------- //

  function on(formId, handler) {
    document.getElementById(formId).addEventListener("submit", (ev) => {
      ev.preventDefault();
      try {
        handler(new FormData(ev.target));
      } catch (e) {
        setStatus(String(e.message || e), true);
        $results.innerHTML = "";
      }
    });
  }

  on("form-vidpid", (fd) => {
    const vid = parseHex4(fd.get("vid"));
    const pid = fd.get("pid") ? parseHex4(fd.get("pid")) : 0;
    const { cols, rows } = runQuery(QUERIES.vidpidToBoards, [vid, pid]);
    setStatus(`vid=0x${vid.toString(16)} pid=0x${pid.toString(16)} → ${rows.length} candidate board(s)`);
    renderTable(cols, rows, "no boards match this VID — try just the VID field");
  });

  on("form-board", (fd) => {
    const q = quoteForFts5(fd.get("q"));
    const { cols, rows } = runQuery(QUERIES.boardNameToVid, [q]);
    setStatus(`fts5 match: ${q} → ${rows.length} ranked VID candidates`);
    renderTable(cols, rows, "no boards match; try a shorter fragment");
  });

  on("form-mcu", (fd) => {
    const { cols, rows } = runQuery(QUERIES.listByMcu, [fd.get("mcu")]);
    setStatus(`mcu=${fd.get("mcu")} → ${rows.length} boards`);
    renderTable(cols, rows, "no boards for that MCU family");
  });

  on("form-vid-products", (fd) => {
    const vid = parseHex4(fd.get("vid"));
    const { cols, rows } = runQuery(QUERIES.productsUnderVid, [vid]);
    setStatus(`vid=0x${vid.toString(16)} → ${rows.length} known products`);
    renderTable(cols, rows, "no products listed under this VID");
  });

  on("form-vendor", (fd) => {
    const like = `%${String(fd.get("q")).replace(/[\\%_]/g, "\\$&")}%`;
    const { cols, rows } = runQuery(QUERIES.vendorSearch, [like]);
    setStatus(`vendor like ${like} → ${rows.length} match(es)`);
    renderTable(cols, rows, "no vendors match");
  });

  // Boot.
  loadDatabase().catch(e => {
    setStatus(`failed to load: ${e.message || e}`, true);
  });
})();
