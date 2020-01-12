use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use futures::future::FutureExt;

struct NeededPost {
    id: i32,
    full_url: String,
}

async fn hash_url(
    client: std::sync::Arc<reqwest::Client>,
    url: String,
) -> Result<(img_hash::ImageHash, i64), image::ImageError> {
    let data = client
        .get(&url)
        .send()
        .await
        .expect("unable to get url")
        .bytes()
        .await
        .expect("unable to get bytes");

    let hasher = furaffinity_rs::get_hasher();
    let image = image::load_from_memory(&data)?;

    let hash = hasher.hash_image(&image);
    let mut bytes: [u8; 8] = [0; 8];
    bytes.copy_from_slice(hash.as_bytes());

    let num = i64::from_be_bytes(bytes);

    println!("{} - {}", url, num);

    Ok((hash, num))
}

#[tokio::main]
async fn main() {
    let dsn = std::env::var("POSTGRES_DSN").expect("missing postgres dsn");

    use std::str::FromStr;
    let manager = PostgresConnectionManager::new(
        tokio_postgres::Config::from_str(&dsn).expect("unable to parse postgres dsn"),
        tokio_postgres::NoTls,
    );

    let pool = Pool::builder()
        .build(manager)
        .await
        .expect("unable to build pool");

    let client = reqwest::Client::builder()
        .user_agent("Syfaro test client syfaro@huefox.com")
        .build()
        .expect("Unable to build http client");
    let client = std::sync::Arc::new(client);

    loop {
        println!("getting next 384 posts");

        let db = pool.clone();

        let needed_posts: Vec<_> = db
            .get()
            .await
            .unwrap()
            .query(
                "SELECT
                    id,
                    data->>'file_url' file_url
                FROM
                    post
                WHERE
                    hash IS NULL AND
                    hash_error IS NULL AND
                    data->>'file_ext' IN ('jpg', 'png') AND
                    data->>'file_url' <> '/images/deleted-preview.png'
                ORDER BY id DESC
                LIMIT 384",
                &[],
            )
            .await
            .expect("Unable to get posts")
            .into_iter()
            .map(|row| NeededPost {
                id: row.get("id"),
                full_url: row.get("file_url"),
            })
            .collect();

        for chunk in needed_posts.chunks(8) {
            let futs = chunk.iter().map(|post| {
                let db = db.clone();
                let client = client.clone();
                let id = post.id;

                hash_url(client, post.full_url.clone()).then(move |res| async move {
                    match res {
                        Ok((_hash, num)) => {
                            db.get()
                                .await
                                .unwrap()
                                .execute("UPDATE post SET hash = $2 WHERE id = $1", &[&id, &num])
                                .await
                                .expect("Unable to update hash in database");
                        }
                        Err(e) => {
                            use std::error::Error;
                            let desc = e.description();
                            println!("hashing error - {}", desc);
                            db.get()
                                .await
                                .unwrap()
                                .execute(
                                    "UPDATE post SET hash_error = $2 WHERE id = $1",
                                    &[&id, &desc],
                                )
                                .await
                                .expect("Unable to update hash in database");
                        }
                    };
                })
            });

            futures::future::join_all(futs).await;
        }
    }
}
