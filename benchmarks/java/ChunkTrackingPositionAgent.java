package net.spicanetwork.rustnative.benchmark;

import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.MessageDigest;
import java.security.ProtectionDomain;
import java.util.HexFormat;
import java.util.jar.JarFile;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassVisitor;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;

/** Version-pinned position and liveness source for the isolated Purpur benchmark. */
public final class ChunkTrackingPositionAgent implements ClassFileTransformer, Opcodes {
    private static final String ENTITY = "net/minecraft/world/entity/Entity";
    private static final String LIVING = "net/minecraft/world/entity/LivingEntity";
    private static final String VECTOR = "net/minecraft/world/phys/Vec3";
    private static final String ACCESSOR = "Lnet/minecraft/network/syncher/EntityDataAccessor;";
    private static final String FEED = "net/spicanetwork/rustnative/benchmark/ChunkTrackingPositionFeed";

    public static void premain(String feedJar, Instrumentation instrumentation) throws Exception {
        instrumentation.appendToBootstrapClassLoaderSearch(new JarFile(feedJar));
        instrumentation.addTransformer(new ChunkTrackingPositionAgent());
    }

    @Override
    public byte[] transform(ClassLoader loader, String name, Class<?> type, ProtectionDomain domain, byte[] bytes) {
        boolean entity = ENTITY.equals(name);
        if (!entity && !LIVING.equals(name)) {
            return null;
        }
        try {
            String expected = entity ? "5eb05c345d8d6f468f0826f5f0e297bc3d550c3036f8767a26c848bce2dcb2f7"
                : "88f33bc76da07b7c17f5ec173685cfcb75387ab0b43ac5d9485cba7a8a2f110f";
            String actual = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes));
            if (!expected.equals(actual)) {
                throw new IllegalStateException("Unexpected Purpur class hash " + actual);
            }
            ClassReader reader = new ClassReader(bytes);
            ClassWriter writer = new ClassWriter(reader, ClassWriter.COMPUTE_MAXS);
            HookVisitor hooks = new HookVisitor(writer, name);
            reader.accept(hooks, 0);
            if (hooks.primary != 1 || hooks.lifecycle == 0) {
                throw new IllegalStateException("Incomplete Purpur movement hooks");
            }
            byte[] transformed = writer.toByteArray();
            ChunkTrackingPositionFeed.installed(entity ? 1 : 2);
            return transformed;
        } catch (Exception error) {
            System.err.println("SHADOW_FAILED " + name + " " + error);
            return null;
        }
    }

    private static final class HookVisitor extends ClassVisitor {
        private final String owner;
        private int primary;
        private int lifecycle;

        HookVisitor(ClassVisitor target, String owner) {
            super(ASM9, target);
            this.owner = owner;
        }

        @Override
        public MethodVisitor visitMethod(int access, String name, String descriptor, String signature, String[] exceptions) {
            MethodVisitor target = super.visitMethod(access, name, descriptor, signature, exceptions);
            return new MethodVisitor(ASM9, target) {
                @Override
                public void visitCode() {
                    super.visitCode();
                    if (LIVING.equals(owner) && name.equals("onSyncedDataUpdated") && descriptor.equals("(" + ACCESSOR + ")V")) {
                        ++primary;
                        loadId(this);
                        visitVarInsn(ALOAD, 1);
                        super.visitFieldInsn(GETSTATIC, LIVING, "DATA_HEALTH_ID", ACCESSOR);
                        visitMethodInsn(INVOKESTATIC, FEED, "health", "(ILjava/lang/Object;Ljava/lang/Object;)V", false);
                    }
                }

                @Override
                public void visitFieldInsn(int opcode, String fieldOwner, String field, String fieldType) {
                    if (opcode == PUTFIELD && ENTITY.equals(owner) && name.equals("setPosRaw")
                        && descriptor.equals("(DDDZ)V") && ENTITY.equals(fieldOwner) && field.equals("position")) {
                        ++primary;
                        loadCoordinate("x");
                        loadCoordinate("z");
                        visitVarInsn(DLOAD, 1);
                        visitVarInsn(DLOAD, 5);
                        visitVarInsn(ALOAD, 0);
                        visitTypeInsn(INSTANCEOF, "net/minecraft/world/entity/Mob");
                        visitMethodInsn(INVOKESTATIC, FEED, "horizontalChanged", "(DDDDZ)Z", false);
                        // Keep the result beneath the receiver and value until the store succeeds.
                        visitInsn(DUP_X2);
                        visitInsn(POP);
                        super.visitFieldInsn(opcode, fieldOwner, field, fieldType);
                        loadId(this);
                        visitMethodInsn(INVOKESTATIC, FEED, "position", "(ZI)V", false);
                        return;
                    }
                    super.visitFieldInsn(opcode, fieldOwner, field, fieldType);
                    if (opcode == PUTFIELD && !name.equals("<init>") && fieldOwner.equals(owner)
                        && (LIVING.equals(owner) && field.equals("dead") || ENTITY.equals(owner) && field.equals("removalReason"))) {
                        ++lifecycle;
                        loadId(this);
                        visitMethodInsn(INVOKESTATIC, FEED, "dirty", "(I)V", false);
                    }
                }

                private void loadCoordinate(String coordinate) {
                    visitVarInsn(ALOAD, 0);
                    super.visitFieldInsn(GETFIELD, ENTITY, "position", "L" + VECTOR + ";");
                    super.visitFieldInsn(GETFIELD, VECTOR, coordinate, "D");
                }
            };
        }

        private static void loadId(MethodVisitor visitor) {
            visitor.visitVarInsn(ALOAD, 0);
            visitor.visitMethodInsn(INVOKEVIRTUAL, ENTITY, "getId", "()I", false);
        }
    }
}
