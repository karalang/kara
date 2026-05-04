// design_studies/parallel_fanout/ParallelFanout.java
//
// Fetch N user records concurrently and print aggregated output.
// Java variant — virtual threads (Java 21+).
//
// Requires Jackson on the classpath.

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.List;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;

import com.fasterxml.jackson.databind.ObjectMapper;

public class ParallelFanout {
    static class User {
        public long id;
        public String name;
        public String email;
    }

    static User fetch(HttpClient client, ObjectMapper mapper, long id) throws Exception {
        String url = "https://jsonplaceholder.typicode.com/users/" + id;
        HttpRequest req = HttpRequest.newBuilder(URI.create(url)).GET().build();
        HttpResponse<String> resp = client.send(req, HttpResponse.BodyHandlers.ofString());
        if (resp.statusCode() / 100 != 2) {
            throw new RuntimeException("http status " + resp.statusCode() + " for id " + id);
        }
        return mapper.readValue(resp.body(), User.class);
    }

    public static void main(String[] args) throws Exception {
        HttpClient client = HttpClient.newHttpClient();
        ObjectMapper mapper = new ObjectMapper();
        List<Long> ids = List.of(1L, 2L, 3L, 4L, 5L);

        try (var executor = Executors.newVirtualThreadPerTaskExecutor()) {
            List<Future<User>> futures = ids.stream()
                .map(id -> executor.submit(() -> fetch(client, mapper, id)))
                .toList();

            for (Future<User> f : futures) {
                User u = f.get();
                System.out.println(u.id + "\t" + u.name + "\t" + u.email);
            }
        }
    }
}
