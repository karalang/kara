// Java/Netty reference impl for the Parallax bench (phase-6 P1).
//
// Netty 4.1 HTTP server + four `CompletableFuture`s on a fixed pool
// for the fan-out. Same provider busy-loop kernel sizes as the Kāra,
// Rust, Go, and Node impls so all six stay apples-to-apples.
//
// **Sleep substitute.** Per the F5 design lock, providers should
// approximate 2/5/8/12 ms latency. Kāra's stdlib has no `sleep_ms`
// so its impl uses CPU-bound busy loops; Java mirrors the busy-loop
// shape (not `Thread.sleep`) at the same iteration counts. README
// footnotes the deviation.
//
// **Fan-out shape.** The four providers run as `CompletableFuture`s
// on a fixed `ExecutorService` sized to `availableProcessors()` — the
// direct analog of Go's goroutines + `WaitGroup` and Rust's
// `tokio::join!`. The Netty event-loop thread never busy-loops: it
// submits the four CPU tasks and writes the response from the
// completion callback (also off the event loop). `writeAndFlush` is
// thread-safe — Netty queues it onto the channel's event loop.
//
// **Path parsing.** The request URI is read but the user_id is hard-
// coded to 1 (the bench load is user_id-invariant since the busy
// loops dominate), matching the Go/Rust/Node impls.
//
// **`BOUND_PORT=<n>` line.** Mirrors Kāra's runtime convention so
// `bench.sh` can use one port-discovery helper across all six impls.

package parallax;

import io.netty.bootstrap.ServerBootstrap;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.Unpooled;
import io.netty.channel.Channel;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelInitializer;
import io.netty.channel.ChannelOption;
import io.netty.channel.EventLoopGroup;
import io.netty.channel.SimpleChannelInboundHandler;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.SocketChannel;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import io.netty.handler.codec.http.DefaultFullHttpResponse;
import io.netty.handler.codec.http.FullHttpRequest;
import io.netty.handler.codec.http.FullHttpResponse;
import io.netty.handler.codec.http.HttpHeaderNames;
import io.netty.handler.codec.http.HttpHeaderValues;
import io.netty.handler.codec.http.HttpObjectAggregator;
import io.netty.handler.codec.http.HttpResponseStatus;
import io.netty.handler.codec.http.HttpServerCodec;
import io.netty.handler.codec.http.HttpUtil;
import io.netty.handler.codec.http.HttpVersion;

import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public final class ParallaxBench {

    // Same constants as every other impl: ≈ 2 / 5 / 8 / 12 ms of work
    // on a modern x86-64 core (see bench/README.md § "What this
    // measures").
    private static final long FETCH_PROFILE_WORK = 700_000L;
    private static final long FETCH_ORDERS_WORK = 4_000_000L;
    private static final long FETCH_NOTIFS_WORK = 1_700_000L;
    private static final long FETCH_RECOMMEND_WORK = 2_700_000L;

    // Shared CPU pool for the provider fan-out. Sized to the core
    // count — the JVM analog of GOMAXPROCS / tokio's worker pool.
    private static final ExecutorService PROVIDERS =
        Executors.newFixedThreadPool(Runtime.getRuntime().availableProcessors());

    // Hash-mix kernel: a step the JIT cannot reduce to closed form (no
    // algebraic identity for `(x*31 + i) mod p`). Same kernel + same
    // constants as the Kāra / Rust / Go impls so the six measure
    // equivalent work. See docs/investigations/bench_robustness.md § G1.
    private static long busyLoop(long n) {
        long x = 1L;
        for (long i = 0; i < n; i++) {
            x = (x * 31L + i) % 1073741789L;
        }
        return x;
    }

    private static String fetchProfileName(long userId) {
        long unused = busyLoop(FETCH_PROFILE_WORK + userId);
        if (unused == Long.MIN_VALUE) {
            // Dead branch — keeps the JIT from eliding the busy loop.
            return "unreachable";
        }
        return "Alice";
    }

    private static long fetchLatestOrderId(long userId) {
        return busyLoop(FETCH_ORDERS_WORK + userId);
    }

    private static long fetchTopNotificationKind(long userId) {
        return busyLoop(FETCH_NOTIFS_WORK + userId);
    }

    private static long fetchTopRecommendationId(long userId) {
        return busyLoop(FETCH_RECOMMEND_WORK + userId);
    }

    // Hand-built JSON — same shape and field order as the Go/Rust
    // structs, no Jackson dependency needed.
    private static String dashboardJson(
        long userId, String name, long orderId, long notifKind, long recId) {
        return "{\"profile\":{\"user_id\":"
            + userId
            + ",\"name\":\""
            + name
            + "\"},\"latest_order\":{\"order_id\":"
            + orderId
            + "},\"top_notification\":{\"kind\":"
            + notifKind
            + "},\"top_recommendation\":{\"item_id\":"
            + recId
            + "}}";
    }

    private static final class DashboardHandler
        extends SimpleChannelInboundHandler<FullHttpRequest> {

        @Override
        protected void channelRead0(ChannelHandlerContext ctx, FullHttpRequest request) {
            // user_id-invariant load (busy loops dominate), matching the
            // other impls. URI read but ignored.
            final long userId = 1L;
            final boolean keepAlive = HttpUtil.isKeepAlive(request);

            CompletableFuture<String> profile =
                CompletableFuture.supplyAsync(() -> fetchProfileName(userId), PROVIDERS);
            CompletableFuture<Long> order =
                CompletableFuture.supplyAsync(() -> fetchLatestOrderId(userId), PROVIDERS);
            CompletableFuture<Long> notif =
                CompletableFuture.supplyAsync(() -> fetchTopNotificationKind(userId), PROVIDERS);
            CompletableFuture<Long> recommend =
                CompletableFuture.supplyAsync(() -> fetchTopRecommendationId(userId), PROVIDERS);

            CompletableFuture.allOf(profile, order, notif, recommend)
                .whenComplete(
                    (v, t) -> {
                        String json =
                            dashboardJson(
                                userId,
                                profile.join(),
                                order.join(),
                                notif.join(),
                                recommend.join());
                        ByteBuf body =
                            Unpooled.wrappedBuffer(json.getBytes(StandardCharsets.UTF_8));
                        FullHttpResponse response =
                            new DefaultFullHttpResponse(
                                HttpVersion.HTTP_1_1, HttpResponseStatus.OK, body);
                        response
                            .headers()
                            .set(HttpHeaderNames.CONTENT_TYPE, "application/json")
                            .setInt(HttpHeaderNames.CONTENT_LENGTH, body.readableBytes());
                        if (keepAlive) {
                            response
                                .headers()
                                .set(HttpHeaderNames.CONNECTION, HttpHeaderValues.KEEP_ALIVE);
                            ctx.writeAndFlush(response);
                        } else {
                            ctx.writeAndFlush(response)
                                .addListener(io.netty.channel.ChannelFutureListener.CLOSE);
                        }
                    });
        }

        @Override
        public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
            ctx.close();
        }
    }

    public static void main(String[] args) throws Exception {
        EventLoopGroup boss = new NioEventLoopGroup(1);
        EventLoopGroup workers = new NioEventLoopGroup();
        try {
            ServerBootstrap bootstrap = new ServerBootstrap();
            bootstrap
                .group(boss, workers)
                .channel(NioServerSocketChannel.class)
                .childHandler(
                    new ChannelInitializer<SocketChannel>() {
                        @Override
                        protected void initChannel(SocketChannel ch) {
                            ch.pipeline()
                                .addLast(new HttpServerCodec())
                                .addLast(new HttpObjectAggregator(64 * 1024))
                                .addLast(new DashboardHandler());
                        }
                    })
                .childOption(ChannelOption.TCP_NODELAY, true)
                .option(ChannelOption.SO_BACKLOG, 1024);

            // Ephemeral port — same convention as the Go/Rust/Kāra impls.
            Channel channel =
                bootstrap.bind(new InetSocketAddress("127.0.0.1", 0)).sync().channel();
            int port = ((InetSocketAddress) channel.localAddress()).getPort();
            System.out.println("BOUND_PORT=" + port);
            System.out.flush();

            channel.closeFuture().sync();
        } finally {
            boss.shutdownGracefully();
            workers.shutdownGracefully();
            PROVIDERS.shutdown();
        }
    }

    private ParallaxBench() {}
}
