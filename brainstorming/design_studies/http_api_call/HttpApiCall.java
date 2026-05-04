// design_studies/http_api_call/HttpApiCall.java
//
// GET a JSON endpoint, parse the response, print rows.
// See design_studies/http_api_call/findings.md for cross-language notes.
//
// Requires Jackson on the classpath. Uses stdlib HttpClient (Java 11+).

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.List;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

public class HttpApiCall {
    static class User {
        public long id;
        public String name;
        public String email;
    }

    public static void main(String[] args) {
        String url = "https://jsonplaceholder.typicode.com/users";
        try {
            HttpClient client = HttpClient.newHttpClient();
            HttpRequest req = HttpRequest.newBuilder(URI.create(url)).GET().build();
            HttpResponse<String> resp = client.send(req, HttpResponse.BodyHandlers.ofString());

            if (resp.statusCode() / 100 != 2) {
                System.err.println("http status: " + resp.statusCode());
                System.exit(1);
            }

            List<User> users = new ObjectMapper().readValue(
                resp.body(),
                new TypeReference<List<User>>() {}
            );

            for (User u : users) {
                System.out.println(u.id + "\t" + u.name + "\t" + u.email);
            }
        } catch (Exception e) {
            System.err.println("error: " + e.getMessage());
            System.exit(1);
        }
    }
}
