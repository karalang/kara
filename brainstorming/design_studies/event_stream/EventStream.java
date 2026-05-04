// design_studies/event_stream/EventStream.java
//
// Read JSON events line-by-line from stdin and print a one-line
// summary for each. Unbounded push-model source — runs until EOF.
//
// Input shape (one per line):
//   {"event": "login", "user": "alice"}
//
// Requires Jackson on the classpath.

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

public class EventStream {
    public static void main(String[] args) throws Exception {
        ObjectMapper mapper = new ObjectMapper();
        BufferedReader reader = new BufferedReader(
            new InputStreamReader(System.in, StandardCharsets.UTF_8)
        );

        String line;
        while ((line = reader.readLine()) != null) {
            String trimmed = line.trim();
            if (trimmed.isEmpty()) continue;

            try {
                JsonNode event = mapper.readTree(trimmed);
                String type = event.get("event").asText();
                String user = event.get("user").asText();
                System.out.println("[" + type + "] " + user);
            } catch (Exception e) {
                System.err.println("bad event: " + trimmed);
            }
        }
    }
}
