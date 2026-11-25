package net.spicanetwork.rustnative.nativebridge;

import net.spicanetwork.rustnative.api.SpicaRustAPI;

import java.lang.reflect.Modifier;
import java.nio.Buffer;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Path;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.logging.Logger;

/** Standalone Java 21 smoke test. Pass the directory containing the built native library. */
public final class ArenaAccessSmoke {
    public static void main(String[] args) throws Exception {
        var arena = new EntityBatchArena(2, 2);
        for (var field : EntityBatchArena.class.getDeclaredFields()) {
            if (Buffer.class.isAssignableFrom(field.getType())) {
                check(Modifier.isPrivate(field.getModifiers()), "Escaping arena buffer: " + field);
            }
        }

        try (var executor = Executors.newSingleThreadExecutor()) {
            var started = new CountDownLatch(1);
            java.util.concurrent.Future<?> setter;
            try (var lease = arena.acquireNativeLease()) {
                executor.submit(() -> {
                    try {
                        lease.close();
                        throw new AssertionError("Foreign thread released native lease");
                    } catch (IllegalMonitorStateException expected) {
                        // The owning thread can still close the lease.
                    }
                }).get(5, TimeUnit.SECONDS);
                setter = executor.submit(() -> {
                    started.countDown();
                    arena.setEntityPositionAndFlags(0, 1, 2, 3, 42);
                });
                check(started.await(5, TimeUnit.SECONDS), "Setter thread did not start");
                try {
                    setter.get(200, TimeUnit.MILLISECONDS);
                    throw new AssertionError("Setter wrote during native lease");
                } catch (TimeoutException expected) {
                    // JNI and Java mutation hold the same lock.
                }
            }
            setter.get(5, TimeUnit.SECONDS);
            check(arena.getFlags(0) == 42, "Setter lost after native lease");

            var firstLease = arena.acquireNativeLease();
            firstLease.close();
            executor.submit(() -> {
                try (var secondLease = arena.acquireNativeLease()) {
                    try {
                        firstLease.close();
                        throw new AssertionError("New arena owner closed the old lease");
                    } catch (IllegalMonitorStateException expected) {
                        // A lease belongs to its own thread, not the next owner of the arena.
                    }
                }
            }).get(5, TimeUnit.SECONDS);
        }

        Path nativeDirectory = Path.of(args[0]).toAbsolutePath();
        System.setProperty("spica.native.dir", nativeDirectory.toString());
        NativeLoader.load(nativeDirectory, Logger.getAnonymousLogger(), true, "2.6.0");
        var api = new SpicaRustAPI();
        arena.withExclusiveAccess(owned -> {
            owned.setEntityPosition(0, 0, 64, 0, 0.5f);
            owned.setTargetPosition(0, 1, 64, 0);
            check(api.computeAiTargets(owned, 1, 1, 32, 56) == 1, "Owned JNI call failed");
            check(owned.getOutIntA(0) == 0, "JNI output was not published under the arena lock");
        });

        var current = direct(8).putInt(0, 31).putInt(4, 5);
        var target = direct(8).putInt(0, 42).putInt(4, 5);
        var x = direct(8).putInt(4, 1234);
        var y = direct(8);
        var z = direct(8);
        var id = direct(8);
        check(api.computeBlockDiff(2, 1, 1, current, target, 2, x, y, z, id) == 1,
            "Staged block diff failed");
        check(x.getInt(0) == 0 && y.getInt(0) == 0 && z.getInt(0) == 0 && id.getInt(0) == 42,
            "Staged block diff produced an incorrect change");
        check(x.getInt(4) == 1234, "Staged diff overwrote unwritten entries");
        check(api.computeBlockDiff(2, 1, 1, current, target, 0, null, null, null, null) == 1,
            "Count-only block diff failed");
        System.out.println("Arena locking, zero-copy owned API and staged block diff passed");
    }

    private static ByteBuffer direct(int bytes) {
        return ByteBuffer.allocateDirect(bytes).order(ByteOrder.nativeOrder());
    }

    private static void check(boolean condition, String failure) {
        if (!condition) {
            throw new AssertionError(failure);
        }
    }
}
