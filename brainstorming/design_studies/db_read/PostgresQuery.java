// design_studies/db_read/PostgresQuery.java
//
// Connect to Postgres and print rows from a `users` table.
// Companion to the Python, Rust (minimal/production), and
// Kāra (direct/injected) variants in this directory.
//
// Requires the postgresql JDBC driver on the classpath.

import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;

public class PostgresQuery {
    public static void main(String[] args) {
        String url = System.getenv("DATABASE_URL");
        if (url == null) {
            System.err.println("DATABASE_URL not set");
            System.exit(1);
        }

        try (Connection conn = DriverManager.getConnection(url);
             PreparedStatement stmt = conn.prepareStatement(
                 "SELECT id, name, email FROM users ORDER BY id");
             ResultSet rs = stmt.executeQuery()) {

            while (rs.next()) {
                long id = rs.getLong("id");
                String name = rs.getString("name");
                String email = rs.getString("email");
                System.out.println(id + "\t" + name + "\t" + email);
            }
        } catch (SQLException e) {
            System.err.println("DB error: " + e.getMessage());
            System.exit(1);
        }
    }
}
