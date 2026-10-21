-- Database bucket vectors describe complete snapshots. Entity revisions are
-- zero-padded decimal strings so comparison never rounds a bigint through Lua.
local meta,rank,members,deadlines,versions=KEYS[1],KEYS[2],KEYS[3],KEYS[4],KEYS[5]
local mode,stamp,now,target=ARGV[1],ARGV[2],tonumber(ARGV[3]),ARGV[4]
local old=redis.call('GET',meta)
local oldstamp,n,d,count,reset,basis
if old then oldstamp,n,d,count,reset,basis=string.match(old,'^([^|]+)|(%d+)|(%d+)|(%d+)|(%d+)|(.+)$') end
local valid=oldstamp and redis.call('ZCARD',rank)==tonumber(n)
    and redis.call('HLEN',members)==tonumber(n)
    and redis.call('ZCARD',deadlines)==tonumber(d)
    and redis.call('HLEN',versions)==tonumber(count)
local function vector(value)
    local result={}
    for generation in string.gmatch(value or '', '[^,]+') do result[#result+1]=generation end
    if #result~=64 then return nil end
    return result
end
local function dominates(new,previous)
    local a,b=vector(new),vector(previous)
    if not a or not b then return false end
    for i=1,64 do if a[i]<b[i] then return false end end
    return true
end
local function save(version,frontier,reset_at)
    redis.call('SET',meta,version..'|'..redis.call('ZCARD',rank)..'|'..redis.call('ZCARD',deadlines)..'|'..redis.call('HLEN',versions)..'|'..reset_at..'|'..frontier,'EX',600)
    for i=2,5 do redis.call('EXPIRE',KEYS[i],600) end
end
if mode=='basis' then
    if not valid or tonumber(reset)>now then return nil end
    return basis
elseif mode=='read' then
    if not valid or oldstamp~=stamp or basis~=target or tonumber(reset)>now then return nil end
    local due=redis.call('ZRANGEBYSCORE',deadlines,'-inf',ARGV[3])
    if #due>0 then
        redis.call('DEL',meta)
        for _,id in ipairs(due) do
            local member=redis.call('HGET',members,id)
            if not member then return nil end
            local zero=string.rep('f',48)..string.sub(member,49)
            redis.call('ZREM',rank,member)
            redis.call('ZADD',rank,0,zero)
            redis.call('HSET',members,id,zero)
            redis.call('ZREM',deadlines,id)
        end
        reset=ARGV[3]
    end
    save(stamp,target,reset)
    return redis.call('ZRANGE',rank,0,9)
end
local start=mode=='apply' and 6 or 5
if mode=='apply' then
    -- Compare-and-swap the entire mirror basis. A later writer or another
    -- instance may already have advanced it while this snapshot was loading.
    if not valid or basis~=ARGV[5] or not dominates(target,basis) then return 0 end
elseif mode=='rebuild' then
    if valid and not dominates(target,basis) then return 0 end
else return redis.error_reply('unknown quota rank operation') end
local incoming={}
for i=start,#ARGV,4 do
    local id,revision=ARGV[i],ARGV[i+1]
    incoming[id]=revision
    local previous=redis.call('HGET',versions,id)
    if previous and previous>revision then return 0 end
end
if mode=='rebuild' then
    -- Retain deletion fences even when another key was evicted. An old
    -- PostgreSQL snapshot must not re-create a removed member at an older rev.
    local existing=redis.call('HGETALL',versions)
    for i=1,#existing,2 do
        local revision=incoming[existing[i]]
        if not revision or revision<existing[i+1] then return 0 end
    end
    redis.call('DEL',meta,rank,members,deadlines,versions)
else redis.call('DEL',meta) end
for i=start,#ARGV,4 do
    local id,revision,member,deadline=ARGV[i],ARGV[i+1],ARGV[i+2],ARGV[i+3]
    local previous=redis.call('HGET',members,id)
    if previous then redis.call('ZREM',rank,previous) end
    redis.call('HDEL',members,id)
    redis.call('ZREM',deadlines,id)
    redis.call('HSET',versions,id,revision)
    if member~='' then
        redis.call('ZADD',rank,0,member)
        redis.call('HSET',members,id,member)
        if deadline~='' then redis.call('ZADD',deadlines,deadline,id) end
    end
end
save(stamp,target,ARGV[3])
return 1
