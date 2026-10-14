import Foundation

@MainActor
private final class Context {
    var time = Date(timeIntervalSince1970: 1000)
    var requests = 0
    var gate: CheckedContinuation<Int, Never>?
}

@main
struct ListPageCacheTests {
    @MainActor
    static func main() async throws {
        let context = Context()
        let cache = ListPageCache(maxEntries: 2, maxBytes: 1024, now: { context.time })
        let first: Int = try await cache.load("nodes:page1", ttl: 15) { bypass in
            precondition(!bypass); context.requests += 1; return 1
        }
        precondition(first == 1)
        let reused: Int = try await cache.load("nodes:page1", ttl: 15) { _ in context.requests += 1; return 2 }
        precondition(reused == 1 && context.requests == 1)
        context.time = context.time.addingTimeInterval(16)
        precondition(cache.peek("nodes:page1", as: Int.self) == 1)
        do {
            let _: Int = try await cache.load("nodes:page1", ttl: 15) { _ in throw URLError(.timedOut) }
            preconditionFailure("failed fetch unexpectedly succeeded")
        } catch {}
        precondition(cache.peek("nodes:page1", as: Int.self) == 1)
        let updated: Int = try await cache.load("nodes:page1", ttl: 15) { _ in context.requests += 1; return 2 }
        precondition(updated == 2 && context.requests == 2)
        let _: Int = try await cache.load("credentials:page1", ttl: 60) { _ in 3 }
        cache.invalidate(types: ["nodes"])
        let untouched: Int = try await cache.load("credentials:page1", ttl: 60) { _ in preconditionFailure("unrelated invalidation") }
        precondition(untouched == 3)
        let forced: Int = try await cache.load("nodes:page1", ttl: 15) { bypass in precondition(bypass); return 4 }
        precondition(forced == 4)

        cache.invalidate(clear: true)
        let a = Task { try await cache.load("jobs:page1", ttl: 15) { _ in
            context.requests += 1
            return await withCheckedContinuation { context.gate = $0 }
        } }
        while context.gate == nil { await Task.yield() }
        let b = Task { try await cache.load("jobs:page1", ttl: 15) { _ in preconditionFailure("duplicate flight") } as Int }
        // Let the second consumer join, then cancel the original page consumer.
        for _ in 0..<10 { await Task.yield() }
        a.cancel()
        cache.invalidate(types: ["credentials"])
        context.gate!.resume(returning: 5); context.gate = nil
        do { _ = try await a.value; preconditionFailure("cancelled consumer") } catch {}
        let merged = try await b.value
        precondition(merged == 5 && context.requests == 3)

        let stale = Task { try await cache.load("old-service", ttl: 15) { _ in
            return await withCheckedContinuation { context.gate = $0 }
        } }
        while context.gate == nil { await Task.yield() }
        cache.invalidate(clear: true)
        context.gate!.resume(returning: 6); context.gate = nil
        do { _ = try await stale.value; preconditionFailure("old connection result") } catch {}
        precondition(cache.peek("old-service", as: Int.self) == nil)
        for index in 1...3 {
            context.time = context.time.addingTimeInterval(1)
            let _: Int = try await cache.load("page\(index)", ttl: 15) { _ in index }
        }
        precondition(cache.peek("page1", as: Int.self) == nil)
        precondition(cache.peek("page2", as: Int.self) == 2)
        let visits = ListPageCache()
        let _: Int = try await visits.load("page", ttl: 60) { _ in 10 }
        let reenter = Task { try await visits.load("page", ttl: 60, revalidate: true) { bypass in
            precondition(bypass)
            return await withCheckedContinuation { context.gate = $0 }
        } }
        while context.gate == nil { await Task.yield() }
        precondition(visits.peek("page", as: Int.self) == 10, "first screen retains the cache during a fresh request")
        context.gate!.resume(returning: 11); context.gate = nil
        let fresh = try await reenter.value
        precondition(fresh == 11 && visits.peek("page", as: Int.self) == 11)
        do {
            let _: Int = try await visits.load("page", ttl: 60, revalidate: true) { _ in throw URLError(.timedOut) }
            preconditionFailure("refresh failure")
        } catch {}
        precondition(visits.peek("page", as: Int.self) == 11)
        let largeCache = ListPageCache(maxBytes: 32)
        let _: String = try await largeCache.load("large", ttl: 15) { _ in String(repeating: "x", count: 64) }
        precondition(largeCache.peek("large", as: String.self) == nil)
        print("Page cache: reuse, expiry, stale-on-error, selective invalidation, forced refresh, single flight, cancellation, connection isolation and bounds passed")
    }
}
