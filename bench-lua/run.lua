-- Lua 5.4 equivalents of the eight programs xtask/src/bench.rs measures,
-- run with the same methodology (one discarded warm-up, five timed
-- samples, median reported, ops/sec = work_units / median_seconds), so
-- the two sets of numbers in docs/design/performance.md's "Compared
-- against Lua" section were produced the same way.
--
-- Run with: lua5.4 bench-lua/run.lua  (or scripts/bench-lua)
--
-- Each program does the same abstract work as its HollowScript
-- counterpart (the same element counts, the same operations: build, map,
-- filter, reduce, sort, encode and decode, allocate and call a closure),
-- written the straightforward way a Lua programmer would write it, not
-- artificially handicapped to match an implementation detail of the other
-- language. The one place this matters for the comparison, not only the
-- style, is list-map-filter-reduce, record-field-access and closure-heavy:
-- HollowScript's `list.append` copies the whole list (its lists are
-- immutable, by design; see docs/design/performance.md), so building an
-- n-element list through n sequential appends is quadratic there, while
-- Lua's `t[#t+1] = v` is amortised O(1), because Lua's tables are
-- ordinary mutable arrays. Both do the same number of append/insert
-- calls; only the cost per call differs, and that difference is real and
-- reported honestly rather than hidden by writing an artificially slow
-- Lua program to match. string-interpolation-loop is the one benchmark
-- where the two languages agree even on complexity: Lua strings are
-- immutable too, so repeated `..` concatenation is quadratic in both.

local SAMPLES = 5

-- ---- the eight programs ----------------------------------------------

local function arithmeticLoop()
	local sum = 0
	local i = 0
	while i < 200000 do
		sum = sum + i * 2 - 1
		i = i + 1
	end
	return sum
end

local function fib(n)
	if n <= 1 then
		return n
	end
	return fib(n - 1) + fib(n - 2)
end
local function recursiveFibonacci()
	return fib(24)
end

local function listMapFilterReduce()
	local numbers = {}
	local i = 0
	while i < 2000 do
		numbers[#numbers + 1] = i
		i = i + 1
	end
	local doubled = {}
	for idx, n in ipairs(numbers) do
		doubled[idx] = n * 2
	end
	local large = {}
	for _, n in ipairs(doubled) do
		if n > 2000 then
			large[#large + 1] = n
		end
	end
	local acc = 0
	for _, n in ipairs(large) do
		acc = acc + n
	end
	return acc
end

local function recordFieldAccess()
	local points = {}
	local i = 0
	while i < 2000 do
		points[#points + 1] = { x = i, y = i * 2 }
		i = i + 1
	end
	local acc = 0
	for _, p in ipairs(points) do
		acc = acc + p.x + p.y
	end
	return acc
end

local function stringInterpolationLoop()
	local text = ""
	local i = 0
	while i < 3000 do
		text = text .. "x"
		i = i + 1
	end
	return #text
end

local function listSort()
	local numbers = {}
	local i = 0
	local seed = 1
	while i < 5000 do
		seed = (seed * 1103515245 + 12345) % 2147483648
		numbers[#numbers + 1] = seed % 1000000
		i = i + 1
	end
	table.sort(numbers, function(a, b)
		return a < b
	end)
	return #numbers
end

-- A minimal, correct JSON encoder and decoder: Lua 5.4 has none built in
-- (unlike HollowScript, section 15.1's `json` is a prelude namespace), so
-- this is the straightforward amount of code a Lua program doing the same
-- round trip needs to write for itself. Handles exactly the shapes this
-- benchmark produces (nested objects, arrays, strings, integers) plus the
-- general JSON grammar for the decode half, so it is a real round trip,
-- not a shortcut that only works on its own output.
local function jsonEncode(value)
	local t = type(value)
	if t == "string" then
		return '"' .. value:gsub('[\\"]', "\\%0") .. '"'
	elseif t == "number" then
		return string.format("%d", value)
	elseif t == "boolean" then
		return tostring(value)
	elseif t == "table" then
		if value.__isArray then
			local parts = {}
			for i, v in ipairs(value) do
				parts[i] = jsonEncode(v)
			end
			return "[" .. table.concat(parts, ",") .. "]"
		end
		local parts = {}
		local n = 0
		for _, key in ipairs(value.__keys) do
			n = n + 1
			parts[n] = jsonEncode(key) .. ":" .. jsonEncode(value[key])
		end
		return "{" .. table.concat(parts, ",") .. "}"
	end
	error("jsonEncode: unsupported type " .. t)
end

local function jsonArray(items)
	items.__isArray = true
	return items
end

local function jsonObject(keys, values)
	local t = { __keys = keys }
	for i, key in ipairs(keys) do
		t[key] = values[i]
	end
	return t
end

-- A small recursive-descent parser over the general JSON grammar
-- (objects, arrays, strings, numbers, true/false/null), enough to decode
-- what jsonEncode produces and to be a genuine parser, not a lookup table
-- keyed by this benchmark's own fixed shape.
local function jsonDecode(text)
	local pos = 1
	local function skipSpace()
		while pos <= #text and text:sub(pos, pos):match("%s") do
			pos = pos + 1
		end
	end
	local parseValue
	local function parseString()
		pos = pos + 1
		local start = pos
		local out = {}
		while text:sub(pos, pos) ~= '"' do
			local c = text:sub(pos, pos)
			if c == "\\" then
				out[#out + 1] = text:sub(pos + 1, pos + 1)
				pos = pos + 2
			else
				out[#out + 1] = c
				pos = pos + 1
			end
		end
		pos = pos + 1
		return table.concat(out)
	end
	local function parseNumber()
		local start = pos
		while text:sub(pos, pos):match("[%d%-%.eE+]") do
			pos = pos + 1
		end
		return tonumber(text:sub(start, pos - 1))
	end
	local function parseArray()
		pos = pos + 1
		local items = {}
		skipSpace()
		if text:sub(pos, pos) == "]" then
			pos = pos + 1
			return jsonArray(items)
		end
		while true do
			skipSpace()
			items[#items + 1] = parseValue()
			skipSpace()
			if text:sub(pos, pos) == "," then
				pos = pos + 1
			else
				break
			end
		end
		skipSpace()
		pos = pos + 1 -- ']'
		return jsonArray(items)
	end
	local function parseObject()
		pos = pos + 1
		local keys = {}
		local values = {}
		skipSpace()
		if text:sub(pos, pos) == "}" then
			pos = pos + 1
			return jsonObject(keys, values)
		end
		while true do
			skipSpace()
			local key = parseString()
			skipSpace()
			pos = pos + 1 -- ':'
			skipSpace()
			keys[#keys + 1] = key
			values[#values + 1] = parseValue()
			skipSpace()
			if text:sub(pos, pos) == "," then
				pos = pos + 1
			else
				break
			end
		end
		skipSpace()
		pos = pos + 1 -- '}'
		return jsonObject(keys, values)
	end
	parseValue = function()
		skipSpace()
		local c = text:sub(pos, pos)
		if c == '"' then
			return parseString()
		elseif c == "[" then
			return parseArray()
		elseif c == "{" then
			return parseObject()
		elseif text:sub(pos, pos + 3) == "true" then
			pos = pos + 4
			return true
		elseif text:sub(pos, pos + 4) == "false" then
			pos = pos + 5
			return false
		elseif text:sub(pos, pos + 3) == "null" then
			pos = pos + 4
			return nil
		else
			return parseNumber()
		end
	end
	return parseValue()
end

local function jsonRoundtrip()
	local entries = {}
	local i = 0
	while i < 1000 do
		local tags = jsonArray({ "a", "b", "c" })
		entries[#entries + 1] =
			jsonObject({ "id", "name", "tags" }, { i, "item-" .. i, tags })
		i = i + 1
	end
	local encoded = jsonEncode(jsonArray(entries))
	local decoded = jsonDecode(encoded)
	if decoded == nil then
		return -1
	end
	return #encoded
end

local function closureHeavy()
	local function makeAdder(n)
		return function(x)
			return x + n
		end
	end
	local adders = {}
	local i = 0
	while i < 4000 do
		adders[#adders + 1] = makeAdder(i)
		i = i + 1
	end
	local total = 0
	for _, f in ipairs(adders) do
		total = total + f(1)
	end
	return total
end

-- ---- the harness -------------------------------------------------------

local BENCHMARKS = {
	{ name = "arithmetic-loop", work_units = 200000, run = arithmeticLoop },
	{ name = "recursive-fibonacci", work_units = 150049, run = recursiveFibonacci },
	{ name = "list-map-filter-reduce", work_units = 2000, run = listMapFilterReduce },
	{ name = "record-field-access", work_units = 2000, run = recordFieldAccess },
	{ name = "string-interpolation-loop", work_units = 3000, run = stringInterpolationLoop },
	{ name = "list-sort", work_units = 5000, run = listSort },
	{ name = "json-roundtrip", work_units = 1000, run = jsonRoundtrip },
	{ name = "closure-heavy", work_units = 4000, run = closureHeavy },
}

local function median(values)
	table.sort(values)
	return values[math.ceil(#values / 2)]
end

local function measureOnce(run)
	local start = os.clock()
	run()
	return os.clock() - start
end

print(string.format("%-26s %14s", "program", "ops/sec"))
for _, bench in ipairs(BENCHMARKS) do
	measureOnce(bench.run) -- one discarded warm-up run
	local samples = {}
	for i = 1, SAMPLES do
		samples[i] = measureOnce(bench.run)
	end
	local seconds = median(samples)
	local opsPerSecond
	if seconds <= 0 then
		-- os.clock()'s resolution is too coarse to time this program
		-- directly; report it as immeasurably fast rather than divide
		-- by zero or a misleadingly precise number.
		opsPerSecond = math.huge
	else
		opsPerSecond = bench.work_units / seconds
	end
	print(string.format("%-26s %14.0f", bench.name, opsPerSecond))
end
