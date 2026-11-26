package net.spicanetwork.rustnative.benchmark;

import com.destroystokyo.paper.event.entity.EntityAddToWorldEvent;
import com.destroystokyo.paper.event.entity.EntityRemoveFromWorldEvent;
import com.destroystokyo.paper.event.server.ServerTickEndEvent;
import com.destroystokyo.paper.event.server.ServerTickStartEvent;
import io.papermc.paper.event.entity.EntityMoveEvent;
import java.util.logging.Logger;
import org.bukkit.Bukkit;
import org.bukkit.Location;
import org.bukkit.World;
import org.bukkit.entity.Pig;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.entity.EntityTeleportEvent;
import org.bukkit.event.server.ServerCommandEvent;

/** Verifies the motion notifications used by a dirty-only tracker against the test fixture. */
final class ChunkTrackingMovementCoverage implements Listener {
    private enum Step { IDLE, CONTROL_TELEPORT, WRITE_POSITION, CHECK_POSITION }

    private static final double RANGE = 48;
    private final Logger logger;
    private Step step = Step.IDLE;
    private Pig pig;
    private Location before;
    private int controlTeleports;
    private int moveEvents;
    private int teleportEvents;
    private int worldEvents;
    private int commandEvents;
    private int candidatesBefore;

    ChunkTrackingMovementCoverage(Logger logger) {
        this.logger = logger;
    }

    void arm() {
        if (step != Step.IDLE) {
            throw new IllegalStateException("Movement fixture is already active");
        }
        pig = Bukkit.getWorlds().getFirst().getEntitiesByClass(Pig.class).stream()
            .filter(entity -> entity.getScoreboardTags().contains("spica_shadow_lifecycle"))
            .findFirst().orElseThrow(() -> new IllegalStateException("Movement fixture pig is missing"));
        step = Step.CONTROL_TELEPORT;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void tickEnd(ServerTickEndEvent event) {
        try {
            switch (step) {
                case CONTROL_TELEPORT -> controlTeleport();
                case WRITE_POSITION -> writePosition();
                default -> { }
            }
        } catch (RuntimeException error) {
            fail(error);
        }
    }

    private void controlTeleport() {
        teleportEvents = 0;
        Player anchor = pig.getWorld().getPlayers().getFirst();
        Location start = anchor.getLocation().add(RANGE - 0.01, 0, 0);
        if (!pig.teleport(start) || teleportEvents == 0) {
            throw new IllegalStateException("Control teleport did not deliver an event");
        }
        controlTeleports = teleportEvents;
        // Allow a complete tick-start drain of dirty entries from the control before the write.
        step = Step.WRITE_POSITION;
    }

    private void writePosition() {
        before = pig.getLocation();
        candidatesBefore = candidates(before);
        if (candidatesBefore != 20) {
            throw new IllegalStateException("Expected all 20 bots to track the fixture candidate");
        }
        if (notifications() != 0) {
            throw new IllegalStateException("Fixture already had a pending dirty notification before the position write");
        }
        double x = before.getX() + 0.02;
        String position = "{Pos:[" + x + "d," + before.getY() + "d," + before.getZ() + "d]}";
        if (!Bukkit.dispatchCommand(Bukkit.getConsoleSender(),
            "minecraft:data merge entity " + pig.getUniqueId() + " " + position)) {
            throw new IllegalStateException("Movement fixture could not write the mob position");
        }
        step = Step.CHECK_POSITION;
    }

    @EventHandler(priority = EventPriority.LOWEST)
    public void tickStart(ServerTickStartEvent event) {
        if (step == Step.WRITE_POSITION) {
            moveEvents = 0;
            teleportEvents = 0;
            worldEvents = 0;
            commandEvents = 0;
            return;
        }
        if (step != Step.CHECK_POSITION) {
            return;
        }
        try {
            Location after = pig.getLocation();
            int candidatesAfter = candidates(after);
            if (!pig.isValid() || pig.isDead() || before.getWorld() != after.getWorld()
                || before.getBlockX() != after.getBlockX() || before.getBlockY() != after.getBlockY()
                || before.getBlockZ() != after.getBlockZ() || after.getX() <= before.getX() || candidatesAfter != 0) {
                throw new IllegalStateException("Position-write fixture did not cross the range boundary inside the same block");
            }
            if (notifications() != 0) {
                throw new IllegalStateException("Position-write fixture received a dirty notification");
            }
            logger.info("SHADOW_MOVEMENT_GAP tick=" + event.getTickNumber() + " x=" + before.getX() + "->" + after.getX()
                + " baseline=" + candidatesBefore + "->" + candidatesAfter + " eventOnly=" + candidatesBefore
                + " controlTeleports=" + controlTeleports + " pending=0 move=0 teleport=0 world=0 command=0");
            step = Step.IDLE;
            pig = null;
            before = null;
        } catch (RuntimeException error) {
            fail(error);
        }
    }

    private int notifications() {
        return moveEvents + teleportEvents + worldEvents + commandEvents;
    }

    private static int candidates(Location location) {
        World world = location.getWorld();
        int count = 0;
        for (Player player : world.getPlayers()) {
            double dx = player.getX() - location.getX();
            double dz = player.getZ() - location.getZ();
            if (dx * dx + dz * dz <= RANGE * RANGE) {
                count++;
            }
        }
        return count;
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void moved(EntityMoveEvent event) {
        if (event.getEntity() == pig) {
            moveEvents++;
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void teleported(EntityTeleportEvent event) {
        if (event.getEntity() == pig) {
            teleportEvents++;
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void added(EntityAddToWorldEvent event) {
        if (event.getEntity() == pig) {
            worldEvents++;
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void removed(EntityRemoveFromWorldEvent event) {
        if (event.getEntity() == pig) {
            worldEvents++;
        }
    }

    @EventHandler(priority = EventPriority.MONITOR)
    public void serverCommand(ServerCommandEvent event) {
        if (step == Step.WRITE_POSITION || step == Step.CHECK_POSITION) {
            commandEvents++;
        }
    }

    private void fail(RuntimeException error) {
        step = Step.IDLE;
        pig = null;
        before = null;
        logger.severe("SHADOW_FAILED " + error);
        throw error;
    }
}
