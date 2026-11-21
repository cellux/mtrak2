local M = {}

-- Built-in macro 02: set the tempo from a Q8.8 BPM value carried in the two
-- argument bytes (integer part, fraction). Emits a standard Set Tempo
-- meta-event, which the engine also uses to update its tempo map.
function M.run(arg1, arg2, ctx)
  local bpm = arg1 * 256 + arg2
  if bpm <= 0 then
    return
  end
  local micros = (60000000 * 256) // bpm
  emit_meta(0x51, { (micros >> 16) & 0xFF, (micros >> 8) & 0xFF, micros & 0xFF })
end

return M
