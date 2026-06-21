-- Neovim headless acceptance driver for ridge-lsp.
--
-- Starts ridge-lsp as a real LSP client over stdio (so the framing, the
-- initialize handshake, capability negotiation, and client-side handling are
-- all exercised the way an editor would), drives the handler matrix against a
-- fixture workspace, and exits 0 only when every check passes.
--
-- Output is TAP-ish: a `1..N` plan, then one `ok N - name` / `not ok N - name`
-- line per check. Run via ../run.sh, which points RIDGE_LSP at the binary and
-- RIDGE_WS at the fixture workspace.

local function run()
  local lsp_bin = os.getenv('RIDGE_LSP')
  local ws = os.getenv('RIDGE_WS')
  if not lsp_bin or not ws then
    io.stdout:write('Bail out! RIDGE_LSP and RIDGE_WS must be set\n')
    io.stdout:flush()
    vim.cmd('cquit 1')
  end

  local main_file = ws .. '/app/src/Main.ridge'
  local errors_file = ws .. '/app/src/Errors.ridge'

  local checks = {}
  local function check(name, ok, detail)
    checks[#checks + 1] = { name = name, ok = ok and true or false, detail = detail }
  end

  -- Pump the event loop until `pred` holds or `timeout_ms` elapses.
  local function wait_until(pred, timeout_ms)
    local waited = 0
    while waited < timeout_ms do
      if pred() then return true end
      vim.wait(100)
      waited = waited + 100
    end
    return pred() and true or false
  end

  -- Open the main buffer and attach the server to it.
  vim.cmd('edit ' .. vim.fn.fnameescape(main_file))
  local bufnr = vim.api.nvim_get_current_buf()
  local client_id = vim.lsp.start({
    name = 'ridge-lsp',
    cmd = { lsp_bin },
    root_dir = ws,
  })

  local function get_client()
    return client_id and vim.lsp.get_client_by_id(client_id) or nil
  end

  local attached = wait_until(function()
    local c = get_client()
    return c ~= nil and c.server_capabilities ~= nil
  end, 15000)
  check('client attaches and initializes', attached)
  if not attached then
    io.stdout:write('Bail out! server never attached\n')
    io.stdout:flush()
    vim.cmd('cquit 1')
  end

  local client = get_client()
  local uri = vim.uri_from_fname(main_file)

  -- One synchronous request; returns (result, err).
  local function req(method, params, timeout_ms)
    local res = vim.lsp.buf_request_sync(bufnr, method, params, timeout_ms or 5000)
    if not res or not res[client_id] then return nil, 'no response' end
    local entry = res[client_id]
    return entry.result, entry.err or entry.error
  end

  -- 0-based byte column where `pat` begins on line `row0`, shifted `plus` bytes
  -- into the match. Lets checks target tokens by text, not brittle columns.
  local function col0(row0, pat, plus)
    local line = vim.api.nvim_buf_get_lines(bufnr, row0, row0 + 1, true)[1]
    local s = line:find(pat)
    assert(s, 'pattern not found on line ' .. row0 .. ': ' .. pat)
    return (s - 1) + (plus or 0)
  end

  -- Position params for the cursor at (row0, byte_col), converted to the
  -- server-negotiated encoding by Neovim itself.
  local function pos(row0, byte_col)
    vim.api.nvim_win_set_cursor(0, { row0 + 1, byte_col })
    local ok, p = pcall(vim.lsp.util.make_position_params, 0, client.offset_encoding)
    if not ok then p = vim.lsp.util.make_position_params() end
    return p
  end

  local function start_line(result)
    local loc = result and (result[1] or result) or nil
    return loc and loc.range and loc.range.start.line or nil
  end

  -- Readiness: documentSymbol returns the outline once the workspace is indexed.
  local ready = wait_until(function()
    local r = req('textDocument/documentSymbol', { textDocument = { uri = uri } }, 3000)
    return type(r) == 'table' and #r > 0
  end, 25000)
  check('workspace indexed (documentSymbol)', ready)

  -- hover over the local `u` -> markup.
  do
    local r, err = req('textDocument/hover', pos(2, col0(2, 'u%.age')))
    local md = r and r.contents
      and (r.contents.value or (type(r.contents) == 'table' and r.contents[1])) or nil
    check('hover returns markup', err == nil and md ~= nil)
  end

  -- definition on the `n` use that sits AFTER a multibyte string literal: only
  -- resolves when UTF-16 column negotiation is correct end to end.
  do
    local r, err = req('textDocument/definition', pos(6, col0(6, '" n', 2)))
    local line = start_line(r)
    check('definition across a UTF-16 column resolves', err == nil and line == 6,
      'got line ' .. tostring(line))
  end

  -- typeDefinition on `u` -> the `User` type declaration (line 0).
  do
    local r, err = req('textDocument/typeDefinition', pos(2, col0(2, 'u%.age')))
    local line = start_line(r)
    check('typeDefinition lands on the type decl', err == nil and line == 0,
      'got line ' .. tostring(line))
  end

  -- field definition: `u.age` -> the `age` field declaration (line 0).
  do
    local r, err = req('textDocument/definition', pos(2, col0(2, 'u%.age', 2)))
    local line = start_line(r)
    check('field definition lands on the field decl', err == nil and line == 0,
      'got line ' .. tostring(line))
  end

  -- references on the `label` use -> at least the declaration and the use.
  do
    local p = pos(6, col0(6, 'label'))
    local r, err = req('textDocument/references', {
      textDocument = { uri = uri },
      position = p.position,
      context = { includeDeclaration = true },
    })
    check('references finds use sites', err == nil and type(r) == 'table' and #r >= 2,
      'count ' .. tostring(r and #r))
  end

  -- rename the `age` field -> a WorkspaceEdit touching the decl and a use.
  do
    local p = pos(2, col0(2, 'u%.age', 2))
    p.newName = 'years'
    local r, err = req('textDocument/rename', p)
    local count = 0
    if r and r.changes then
      for _, edits in pairs(r.changes) do count = count + #edits end
    elseif r and r.documentChanges then
      for _, dc in ipairs(r.documentChanges) do count = count + #(dc.edits or {}) end
    end
    check('rename returns a multi-site WorkspaceEdit', err == nil and count >= 2,
      'edits ' .. tostring(count))
  end

  -- documentHighlight on the `label` use -> at least one range.
  do
    local r, err = req('textDocument/documentHighlight', pos(6, col0(6, 'label')))
    check('documentHighlight returns ranges', err == nil and type(r) == 'table' and #r >= 1,
      'count ' .. tostring(r and #r))
  end

  -- semanticTokens/full -> a non-empty, well-formed stream (5 ints per token).
  do
    local r, err = req('textDocument/semanticTokens/full', { textDocument = { uri = uri } })
    local data = r and r.data or nil
    check('semanticTokens/full returns a valid stream',
      err == nil and type(data) == 'table' and #data > 0 and (#data % 5 == 0),
      'len ' .. tostring(data and #data))
  end

  -- formatting and inlayHint: a clean response (an empty result is legitimate).
  do
    local _, err = req('textDocument/formatting', {
      textDocument = { uri = uri },
      options = { tabSize = 2, insertSpaces = true },
    })
    check('formatting responds without error', err == nil)
  end
  do
    local _, err = req('textDocument/inlayHint', {
      textDocument = { uri = uri },
      range = { start = { line = 0, character = 0 }, ['end'] = { line = 6, character = 0 } },
    })
    check('inlayHint responds without error', err == nil)
  end

  -- diagnostics: a file with a type error publishes a diagnostic to the client.
  do
    vim.cmd('edit ' .. vim.fn.fnameescape(errors_file))
    local ebuf = vim.api.nvim_get_current_buf()
    local has = wait_until(function() return #vim.diagnostic.get(ebuf) > 0 end, 15000)
    check('diagnostics published for a type error', has,
      'count ' .. tostring(#vim.diagnostic.get(ebuf)))
  end

  -- Report.
  local failed = 0
  io.stdout:write('1..' .. #checks .. '\n')
  for i, c in ipairs(checks) do
    if not c.ok then failed = failed + 1 end
    io.stdout:write((c.ok and 'ok ' or 'not ok ') .. i .. ' - ' .. c.name
      .. (c.detail and (' # ' .. tostring(c.detail)) or '') .. '\n')
  end
  io.stdout:write((failed == 0 and '# all passed\n' or ('# ' .. failed .. ' failed\n')))
  io.stdout:flush()
  vim.cmd('cquit ' .. (failed > 0 and 1 or 0))
end

local ok, err = pcall(run)
if not ok then
  io.stdout:write('Bail out! ' .. tostring(err) .. '\n')
  io.stdout:flush()
  vim.cmd('cquit 1')
end
