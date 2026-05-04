// design_studies/json_read/ReadUsers.java
//
// Read a JSON array of users from disk and print rows.
// Usage: java ReadUsers <path>
//
// Requires Jackson on the classpath:
//   com.fasterxml.jackson.core:jackson-databind

import java.io.File;
import java.io.IOException;
import java.util.List;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

public class ReadUsers {
    static class User {
        public long id;
        public String name;
        public String email;
    }

    public static void main(String[] args) {
        if (args.length < 1) {
            System.err.println("usage: ReadUsers <path>");
            System.exit(1);
        }

        try {
            ObjectMapper mapper = new ObjectMapper();
            List<User> users = mapper.readValue(
                new File(args[0]),
                new TypeReference<List<User>>() {}
            );

            for (User u : users) {
                System.out.println(u.id + "\t" + u.name + "\t" + u.email);
            }
        } catch (IOException e) {
            System.err.println("error: " + e.getMessage());
            System.exit(1);
        }
    }
}
