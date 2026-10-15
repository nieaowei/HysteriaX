-- All members have score zero. Exact ratio + name + ID lexicographic keys avoid
-- IEEE-754 rounding changing a rank at large byte counters.
local meta, rank, members, deadlines = KEYS[1], KEYS[2], KEYS[3], KEYS[4]
local mode, version, now = ARGV[1], ARGV[2], tonumber(ARGV[3])
local old = redis.call('GET', meta)
local v, n, d, reset
if old then v,n,d,reset=string.match(old,'^(%d+):(%d+):(%d+):(%d+)$') end
local valid = v and redis.call('ZCARD',rank)==tonumber(n)
    and redis.call('HLEN',members)==tonumber(n)
    and redis.call('ZCARD',deadlines)==tonumber(d)
local function save(revision, reset_at)
    redis.call('SET',meta,revision..':'..redis.call('ZCARD',rank)..':'..redis.call('ZCARD',deadlines)..':'..reset_at,'EX',600)
    for i=2,4 do redis.call('EXPIRE',KEYS[i],600) end
end
if mode=='read' then
    if not valid or v~=version or tonumber(reset)>now then return nil end
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
    save(v,reset)
    return redis.call('ZRANGE',rank,0,9)
elseif mode=='rebuild' then
    -- A request holding an older PostgreSQL snapshot must not replace newer data.
    if valid and v>version then return 0 end
    redis.call('DEL',meta,rank,members,deadlines)
elseif mode=='update' then
    if valid and v>=version then return 1 end
    if not valid or v~=ARGV[4] then return 0 end
    redis.call('DEL',meta)
    local id=ARGV[5]
    local previous=redis.call('HGET',members,id)
    if previous then redis.call('ZREM',rank,previous) end
    redis.call('HDEL',members,id)
    redis.call('ZREM',deadlines,id)
else return redis.error_reply('unknown quota rank operation') end
local start=mode=='rebuild' and 4 or 5
for i=start,#ARGV,3 do
    local id,member,deadline=ARGV[i],ARGV[i+1],ARGV[i+2]
    if member~='' then
        redis.call('ZADD',rank,0,member)
        redis.call('HSET',members,id,member)
        if deadline~='' then redis.call('ZADD',deadlines,deadline,id) end
    end
end
save(version,ARGV[3])
return 1
