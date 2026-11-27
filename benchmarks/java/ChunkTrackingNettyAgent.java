package net.spicanetwork.rustnative.benchmark;

import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.MessageDigest;
import java.security.ProtectionDomain;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassVisitor;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.Label;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;

/** Wraps the selected scopes in the pinned Purpur/Netty test build. */
public final class ChunkTrackingNettyAgent implements ClassFileTransformer, Opcodes {
    private static final String CLOCK = "net/spicanetwork/rustnative/benchmark/ChunkTrackingNettyClock";
    private static final String WORKER = CLOCK + "$Worker";
    private static final String BUFFER = "io/netty/buffer/ByteBuf";
    private static final String BYTE_BUF = "L" + BUFFER + ";";
    private static final String CONTEXT = "Lio/netty/channel/ChannelHandlerContext;";
    private static final String BUFFER_ENCODE = "(" + CONTEXT + BYTE_BUF + BYTE_BUF + ")V";

    private record Hook(String name, String descriptor) {}

    private record Stage(int id, String hash, List<Hook> hooks) {
        Stage(int id, String hash, Hook... hooks) {
            this(id, hash, List.of(hooks));
        }
    }

    private static final Map<String, Stage> STAGES = Map.of(
        "net/minecraft/network/PacketEncoder", new Stage(ChunkTrackingNettyClock.ENCODE,
            "be2f670051419d20f11f36d1b65d7fe93fdb699abba5ce88bfdf24464e5be02f",
            new Hook("encode", "(" + CONTEXT + "Lnet/minecraft/network/protocol/Packet;" + BYTE_BUF + ")V")),
        "net/minecraft/network/CompressionEncoder", new Stage(ChunkTrackingNettyClock.COMPRESSION,
            "8f073637a232334c013a13ae8dcdfc5eaa57ad4a8ffbf03fb757d72b4bb5bba4", new Hook("encode", BUFFER_ENCODE)),
        "io/netty/channel/AbstractChannel$AbstractUnsafe", new Stage(ChunkTrackingNettyClock.FLUSH,
            "8fd67e0d8d6b840e9dec00279236ff9ba36b262444076ce0b4541e1e3a6d6624", new Hook("flush0", "()V")),
        "io/netty/channel/nio/NioEventLoop", new Stage(ChunkTrackingNettyClock.EVENT_LOOP,
            "6af552218ba87ef5278e79f9a437e53ff5d7e760b3010c9c1ceceee4299507fb", new Hook("select", "(J)I"),
            new Hook("selectNow", "()I"), new Hook("selectAgain", "()V"), new Hook("processSelectedKeys", "()V")),
        "io/netty/util/concurrent/SingleThreadEventExecutor", new Stage(ChunkTrackingNettyClock.QUEUE,
            "4195ecd21bf4415fc8bfb46bb7534bef41513cfa97235736730d2faefd220436",
            new Hook("pollTaskFrom", "(Ljava/util/Queue;)Ljava/lang/Runnable;"), new Hook("offerTask", "(Ljava/lang/Runnable;)Z")),
        "io/netty/channel/ChannelOutboundBuffer", new Stage(ChunkTrackingNettyClock.QUEUE,
            "cac310a8d212032bc5eb35e3435d2d2adbcf0aa673f06de8f7a0205c2cd382ca", new Hook("addFlush", "()V"),
            new Hook("addMessage", "(Ljava/lang/Object;ILio/netty/channel/ChannelPromise;)V")),
        "io/netty/buffer/PooledByteBufAllocator", new Stage(ChunkTrackingNettyClock.BUFFER,
            "17bdbd7ba7e10863971926b4d47c59da75e40bd28b6a1332846840936b792f4b",
            new Hook("newHeapBuffer", "(II)" + BYTE_BUF), new Hook("newDirectBuffer", "(II)" + BYTE_BUF)),
        "io/netty/buffer/AbstractReferenceCountedByteBuf", new Stage(ChunkTrackingNettyClock.BUFFER,
            "dd0e7577506300d3c7405fffec1ff1af215eafbf63c662866645884fead14c86", new Hook("release", "()Z"),
            new Hook("release", "(I)Z"), new Hook("retain", "()" + BYTE_BUF), new Hook("retain", "(I)" + BYTE_BUF)),
        "net/minecraft/network/Varint21LengthFieldPrepender", new Stage(ChunkTrackingNettyClock.BUFFER,
            "57024e7cb22cc4a46b4b58e63266682bfa516d0cbc6fb06354453fd37647b2eb", new Hook("encode", BUFFER_ENCODE))
    );

    public static void premain(String nativeLibrary, Instrumentation instrumentation) {
        ChunkTrackingNettyClock.load(nativeLibrary);
        instrumentation.addTransformer(new ChunkTrackingNettyAgent());
    }

    @Override
    public byte[] transform(ClassLoader loader, String owner, Class<?> type, ProtectionDomain domain, byte[] bytes) {
        Stage stage = owner == null ? null : STAGES.get(owner);
        if (stage == null) {
            return null;
        }
        try {
            String hash = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes));
            if (!stage.hash().equals(hash)) {
                throw new IllegalStateException("Unexpected class hash " + hash);
            }
            ClassReader reader = new ClassReader(bytes);
            ClassWriter writer = new ClassWriter(reader, ClassWriter.COMPUTE_MAXS);
            WrapVisitor visitor = new WrapVisitor(writer, owner, stage);
            reader.accept(visitor, 0);
            if (visitor.wrapped != stage.hooks().size()) {
                throw new IllegalStateException("Expected " + stage.hooks().size() + " methods, wrapped " + visitor.wrapped);
            }
            System.out.println("SHADOW_NETTY_HOOK " + stage.id() + " " + owner);
            return writer.toByteArray();
        } catch (Exception error) {
            System.err.println("SHADOW_FAILED " + owner + " " + error);
            return null;
        }
    }

    private static final class WrapVisitor extends ClassVisitor {
        private final String owner;
        private final Stage stage;
        private int wrapped;

        WrapVisitor(ClassVisitor target, String owner, Stage stage) {
            super(ASM9, target);
            this.owner = owner;
            this.stage = stage;
        }

        @Override
        public MethodVisitor visitMethod(int access, String name, String descriptor, String signature, String[] exceptions) {
            if (!stage.hooks().contains(new Hook(name, descriptor))) {
                return super.visitMethod(access, name, descriptor, signature, exceptions);
            }
            String original = "spica$" + name;
            MethodVisitor wrapper = super.visitMethod(access, name, descriptor, signature, exceptions);
            int staticFlag = access & ACC_STATIC;
            emitWrapper(wrapper, original, descriptor, staticFlag != 0);
            ++wrapped;
            return super.visitMethod(ACC_PRIVATE | ACC_SYNTHETIC | staticFlag, original, descriptor, signature, exceptions);
        }

        private void emitWrapper(MethodVisitor method, String original, String descriptor, boolean isStatic) {
            Type[] args = Type.getArgumentTypes(descriptor);
            int firstArg = isStatic ? 0 : 1;
            Object[] locals = new Object[firstArg + args.length + 1];
            int workerSlot = firstArg;
            if (!isStatic) {
                locals[0] = owner;
            }
            for (int i = 0; i < args.length; i++) {
                locals[firstArg + i] = frameType(args[i]);
                workerSlot += args[i].getSize();
            }
            locals[locals.length - 1] = WORKER;
            Label begin = new Label();
            Label end = new Label();
            Label cleanup = new Label();
            method.visitCode();
            method.visitTryCatchBlock(begin, end, cleanup, null);
            if (stage.id() == ChunkTrackingNettyClock.COMPRESSION) {
                method.visitVarInsn(ALOAD, 2);
                method.visitMethodInsn(INVOKEVIRTUAL, BUFFER, "readableBytes", "()I", false);
                method.visitVarInsn(ALOAD, 0);
                method.visitFieldInsn(GETFIELD, owner, "threshold", "I");
                method.visitMethodInsn(INVOKESTATIC, CLOCK, "enterCompression", "(II)L" + WORKER + ";", false);
            } else {
                method.visitInsn(ICONST_0 + stage.id());
                method.visitMethodInsn(INVOKESTATIC, CLOCK, "enter", "(I)L" + WORKER + ";", false);
            }
            method.visitVarInsn(ASTORE, workerSlot);
            method.visitLabel(begin);
            if (!isStatic) {
                method.visitVarInsn(ALOAD, 0);
            }
            int slot = firstArg;
            for (Type arg : args) {
                method.visitVarInsn(arg.getOpcode(ILOAD), slot);
                slot += arg.getSize();
            }
            method.visitMethodInsn(isStatic ? INVOKESTATIC : INVOKESPECIAL, owner, original, descriptor, false);
            method.visitLabel(end);
            callExit(method, workerSlot);
            method.visitInsn(Type.getReturnType(descriptor).getOpcode(IRETURN));
            method.visitLabel(cleanup);
            method.visitFrame(F_FULL, locals.length, locals, 1, new Object[]{"java/lang/Throwable"});
            callExit(method, workerSlot);
            method.visitInsn(ATHROW);
            method.visitMaxs(0, 0);
            method.visitEnd();
        }

        private static Object frameType(Type argument) {
            return switch (argument.getSort()) {
                case Type.INT -> INTEGER;
                case Type.LONG -> LONG;
                case Type.OBJECT -> argument.getInternalName();
                default -> throw new IllegalArgumentException("Unexpected argument in pinned method " + argument);
            };
        }

        private static void callExit(MethodVisitor method, int slot) {
            method.visitVarInsn(ALOAD, slot);
            method.visitMethodInsn(INVOKESTATIC, CLOCK, "exit", "(L" + WORKER + ";)V", false);
        }
    }
}
