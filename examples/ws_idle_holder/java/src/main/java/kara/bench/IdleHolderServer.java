package kara.bench;

import io.netty.bootstrap.ServerBootstrap;
import io.netty.channel.Channel;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelInitializer;
import io.netty.channel.ChannelOption;
import io.netty.channel.ChannelPipeline;
import io.netty.channel.EventLoopGroup;
import io.netty.channel.SimpleChannelInboundHandler;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.SocketChannel;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import io.netty.handler.codec.http.HttpObjectAggregator;
import io.netty.handler.codec.http.HttpServerCodec;
import io.netty.handler.codec.http.websocketx.BinaryWebSocketFrame;
import io.netty.handler.codec.http.websocketx.TextWebSocketFrame;
import io.netty.handler.codec.http.websocketx.WebSocketFrame;
import io.netty.handler.codec.http.websocketx.WebSocketServerProtocolHandler;
import io.netty.handler.ssl.SslContext;
import io.netty.handler.ssl.SslContextBuilder;
import io.netty.handler.ssl.SslProvider;

import java.io.InputStream;
import java.net.InetSocketAddress;

/**
 * ws_idle_holder Java comparator — raw Netty WebSocket-over-TLS server that
 * holds N idle connections so the shared bench harness can measure
 * per-connection memory (density), connect latency, and churn against Kāra.
 *
 * <p>This is "Netty raw" — the high-density Java WebSocket prod default that
 * LinkedIn-tier shops deploy directly, NOT Spring / Vert.x / Akka (those are
 * distinct framework-tier comparators, out of scope). The pipeline is the
 * idiomatic minimal stack: SslHandler &rarr; HttpServerCodec &rarr;
 * HttpObjectAggregator &rarr; WebSocketServerProtocolHandler &rarr; echo.
 *
 * <p>Mirrors the Kāra demo + the Go/Rust comparators end-to-end: bind
 * 127.0.0.1:0, print {@code BOUND_PORT=<n>} on stdout for the harness's
 * {@code --server-bin} contract, accept WS-over-TLS at {@code /}, echo
 * text/binary frames, hold idle.
 */
public final class IdleHolderServer {

    public static void main(String[] args) throws Exception {
        // TLS: JDK JSSE SSLEngine (the zero-native-dependency default), TLS
        // 1.2 + 1.3, no client auth, single self-signed cert — the same
        // tests/fixtures/tls fixture (CN=localhost) bundled into the jar as a
        // classpath resource so this comparator is self-contained on a rig.
        // (OpenSSL via netty-tcnative is the non-default perf alternative —
        // see README "TLS provider".)
        SslContext ssl;
        try (InputStream cert = resource("/cert.pem");
             InputStream key = resource("/key.pem")) {
            ssl = SslContextBuilder.forServer(cert, key)
                    .sslProvider(SslProvider.JDK)
                    .protocols("TLSv1.3", "TLSv1.2")
                    .build();
        }

        // Boss accepts; workers run the per-connection pipelines. Worker count
        // defaults to 2 * available processors (Netty default) — prod default,
        // no tuning per the apples-to-apples discipline.
        EventLoopGroup boss = new NioEventLoopGroup(1);
        EventLoopGroup workers = new NioEventLoopGroup();
        try {
            ServerBootstrap b = new ServerBootstrap();
            b.group(boss, workers)
                    .channel(NioServerSocketChannel.class)
                    // Match the Rust comparator's explicit listen(65535); the
                    // kernel clamps to net.core.somaxconn (65535 on the rig).
                    .option(ChannelOption.SO_BACKLOG, 65535)
                    .childOption(ChannelOption.TCP_NODELAY, true)
                    .childHandler(new ChannelInitializer<SocketChannel>() {
                        @Override
                        protected void initChannel(SocketChannel ch) {
                            ChannelPipeline p = ch.pipeline();
                            p.addLast(ssl.newHandler(ch.alloc()));
                            p.addLast(new HttpServerCodec());
                            p.addLast(new HttpObjectAggregator(64 * 1024));
                            // Upgrade WS at "/". The harness sends
                            // `GET / HTTP/1.1` with the RFC 6455 headers.
                            p.addLast(new WebSocketServerProtocolHandler("/"));
                            p.addLast(new EchoFrameHandler());
                        }
                    });

            Channel server = b.bind(new InetSocketAddress("127.0.0.1", 0)).sync().channel();
            int port = ((InetSocketAddress) server.localAddress()).getPort();
            // The harness reads this line to learn the ephemeral port.
            System.out.println("BOUND_PORT=" + port);
            System.out.flush();
            System.err.println("[netty] up on https://127.0.0.1:" + port);

            server.closeFuture().sync();
        } finally {
            boss.shutdownGracefully();
            workers.shutdownGracefully();
        }
    }

    private static InputStream resource(String path) {
        InputStream in = IdleHolderServer.class.getResourceAsStream(path);
        if (in == null) {
            throw new IllegalStateException("missing classpath resource: " + path);
        }
        return in;
    }

    /**
     * Echoes TEXT / BINARY frames (mirrors the Kāra demo's unconditional
     * echo). Idle connections never send, so this is dormant during the
     * density hold; it only fires in the harness's active-traffic phase.
     * Ping/pong/close are handled by {@link WebSocketServerProtocolHandler}.
     */
    static final class EchoFrameHandler extends SimpleChannelInboundHandler<WebSocketFrame> {
        @Override
        protected void channelRead0(ChannelHandlerContext ctx, WebSocketFrame frame) {
            if (frame instanceof TextWebSocketFrame || frame instanceof BinaryWebSocketFrame) {
                ctx.writeAndFlush(frame.retainedDuplicate());
            }
        }

        @Override
        public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
            ctx.close();
        }
    }

    private IdleHolderServer() {
    }
}
