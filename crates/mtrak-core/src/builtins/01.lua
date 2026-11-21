local M = {}

-- Built-in macro 01: trigger the pattern named by argument 1 on the invoking
-- track. Argument 2 is ignored.
function M.run(arg1, arg2, ctx)
  trigger(arg1)
end

return M
