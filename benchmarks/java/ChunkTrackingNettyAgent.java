package net.spicanetwork.rustnative.benchmark;

import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.MessageDigest;
import java.security.ProtectionDomain;
import java.util.HexFormat;
import java.util.Map;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassVisitor;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.Label;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;

/** Only the three methods in the pinned Purpur/Netty test build are wrapped. */
public final class ChunkTrackingNettyAgent implements ClassFileTransformer, Opcodes {
    private static final String CLOCK = "net/spicanetwork/rustnative/benchmark/ChunkTrackingNettyClock";
    private static final String WORKER = CLOCK + "$Worker";
    private static final String BUFFER = "io/netty/buffer/ByteBuf";
    private static final String CONTEXT = "Lio/netty/channel/ChannelHandlerContext;";

    private record Stage(int id, String name, String descriptor, String hash) {}

    private static final Map<String, Stage> STAGES = Map.of(
        "net/minecraft/network/PacketEncoder", new Stage(ChunkTrackingNettyClock.ENCODE, "encode",
            "(" + CONTEXT + "Lnet/minecraft/network/protocol/Packet;L" + BUFFER + ";)V",
            "be2f670051419d20f11f36d1b65d7fe93fdb699abba5ce88bfdf24464e5be02f"),
        "net/minecraft/network/CompressionEncoder", new Stage(ChunkTrackingNettyClock.COMPRESSION, "encode",
            "(" + CONTEXT + "L" + BUFFER + ";L" + BUFFER + ";)V",
            "8f073637a232334c013a13ae8dcdfc5eaa57ad4a8ffbf03fb757d72b4bb5bba4"),
        "io/netty/channel/AbstractChannel$AbstractUnsafe", new Stage(ChunkTrackingNettyClock.FLUSH, "flush0", "()V",
            "8fd67e0d8d6b840e9dec00279236ff9ba36b262444076ce0b4541e1e3a6d6624")
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
            if (!visitor.wrapped) {
                throw new IllegalStateException("Missing pinned method " + stage.name());
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
        private boolean wrapped;

        WrapVisitor(ClassVisitor target, String owner, Stage stage) {
            super(ASM9, target);
            this.owner = owner;
            this.stage = stage;
        }

        @Override
        public MethodVisitor visitMethod(int access, String name, String descriptor, String signature, String[] exceptions) {
            if (!stage.name().equals(name) || !stage.descriptor().equals(descriptor)) {
                return super.visitMethod(access, name, descriptor, signature, exceptions);
            }
            String original = "spica$" + name;
            MethodVisitor wrapper = super.visitMethod(access, name, descriptor, signature, exceptions);
            emitWrapper(wrapper, original);
            wrapped = true;
            return super.visitMethod(ACC_PRIVATE | ACC_SYNTHETIC, original, descriptor, signature, exceptions);
        }

        private void emitWrapper(MethodVisitor method, String original) {
            Type[] args = Type.getArgumentTypes(stage.descriptor());
            int workerSlot = args.length + 1;
            Object[] locals = new Object[workerSlot + 1];
            locals[0] = owner;
            locals[workerSlot] = WORKER;
            for (int i = 0; i < args.length; i++) {
                locals[i + 1] = args[i].getInternalName();
            }
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
            for (int i = 0; i < workerSlot; i++) {
                method.visitVarInsn(ALOAD, i);
            }
            method.visitMethodInsn(INVOKESPECIAL, owner, original, stage.descriptor(), false);
            method.visitLabel(end);
            callExit(method, workerSlot);
            method.visitInsn(RETURN);
            method.visitLabel(cleanup);
            method.visitFrame(F_FULL, locals.length, locals, 1, new Object[]{"java/lang/Throwable"});
            callExit(method, workerSlot);
            method.visitInsn(ATHROW);
            method.visitMaxs(0, 0);
            method.visitEnd();
        }

        private static void callExit(MethodVisitor method, int slot) {
            method.visitVarInsn(ALOAD, slot);
            method.visitMethodInsn(INVOKESTATIC, CLOCK, "exit", "(L" + WORKER + ";)V", false);
        }
    }
}
