package org.cs2world.client;

import java.io.DataInputStream;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** Executed by Android's app_process under the shell UID, outside the VPN application. */
public final class ExternalProbe {
    public static void main(String[] args) {
        try { probe(args); }
        catch (Throwable failure) {
            failure.printStackTrace(System.out);
            System.out.flush();
            System.exit(2);
        }
    }
    private static void probe(String[] args) throws Exception {
        int uid = android.os.Process.myUid();
        if (uid != 2000) throw new AssertionError("expected shell UID; actual " + uid);
        InetAddress target = InetAddress.getByName(args[0]);
        byte[] data = ("probe-" + args[3]).getBytes(StandardCharsets.UTF_8);
        try (Socket tcp = new Socket()) {
            tcp.connect(new InetSocketAddress(target, Integer.parseInt(args[1])), 7000);
            tcp.setSoTimeout(7000); tcp.getOutputStream().write(data);
            byte[] reply = new byte[data.length];
            new DataInputStream(tcp.getInputStream()).readFully(reply);
            if (!Arrays.equals(data, reply)) throw new AssertionError("TCP payload");
        }
        System.out.println("TCP PASS uid=" + uid);
        try (DatagramSocket udp = new DatagramSocket()) {
            int port = Integer.parseInt(args[2]); udp.setSoTimeout(7000);
            udp.send(new DatagramPacket(data, data.length, target, port));
            DatagramPacket result = new DatagramPacket(new byte[2048], 2048);
            udp.receive(result);
            if (!target.equals(result.getAddress()) || port != result.getPort() ||
                    !Arrays.equals(data, Arrays.copyOf(result.getData(), result.getLength())))
                throw new AssertionError("UDP source or payload");
        }
        System.out.println("UDP PASS uid=" + uid);
    }
}
